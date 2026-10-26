// accept-t19.test.js: T-19 acceptance (tester) for the JS half: docs/05 Acceptance 1, the
// in-page halves of 2 and 5, the index side of 6, and B-5 (password never read, isolation,
// fixtures). Real Chromium through playwright-core; injected.js installed exactly as sb-driver
// installs it (helpers.openSb: Page.addScriptToEvaluateOnNewDocument, world "sb").
// Measurements are printed as TAP diagnostics prefixed "ACCEPT".
//
//   node --test injected/test/accept-t19.test.js
"use strict";

const { test, before, after } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const crypto = require("node:crypto");
const { spawnSync } = require("node:child_process");
const { SRC, MEDIA_SRC, serve, launch, openSb, byName, fill } = require("./helpers");

let browser;
let A;
let B;
const opened = [];
const DUMP = path.join(os.tmpdir(), "sb-accept-T-19-js");

before(async () => {
  [A, B] = await Promise.all([serve(), serve()]);
  browser = await launch();
  fs.mkdirSync(DUMP, { recursive: true });
});
after(async () => {
  for (const o of opened) await o.close().catch(() => undefined);
  if (browser) await browser.close();
  if (A) await A.close();
  if (B) await B.close();
});

// After load, the first based walk sb-driver runs per document (D-405 (2): before it the API
// returns {why: "no base"}). opts.base sets the base (default 1); null skips the walk.
async function open(name, opts = {}) {
  const o = await openSb(browser, (/^[a-z]+:/.test(name) ? "" : A.origin + "/") + name, opts);
  opened.push(o);
  if (opts.base !== null) await o.ev((b) => globalThis.__sb.walk({ base: b }), opts.base || 1);
  return o;
}

const timedAct = (o, id) => o.ev(async (i) => {
  const t = performance.now();
  const r = await globalThis.__sb.act(i);
  const end = performance.now();
  return { r, ms: end - t, end };
}, id);

const timedSettle = (o, opts) => o.ev(async (op) => {
  const t = performance.now();
  const r = await globalThis.__sb.settle(op || undefined);
  return { r, ms: performance.now() - t };
}, opts);

// D-442 (B-415 accepted): a field the parser added to a form within 2 s before a one-field
// password swap in that form is over-marked (reads empty|filled). Controls that check an
// ordinary field of the same form reads its text swap after that window. Test-only wait: the
// window is a timestamp in injected.js with no event at its end, so the condition is the page
// clock itself (2000 ms after the load event, which follows every parser insert, plus 50 ms).
// With since (a page performance.now() taken after a script added fields), 2050 ms after it:
// D-443 (4) masks a field added next to a live password for 2 s from its add.
const pastBornWindow = (o, since) => o.page.waitForFunction((s) => {
  if (typeof s === "number") return performance.now() > s + 2050;
  const n = performance.getEntriesByType("navigation")[0];
  return !!n && n.loadEventEnd > 0 && performance.now() > n.loadEventEnd + 2050;
}, typeof since === "number" ? since : null, { polling: 50, timeout: 15000 });

const sig = (r) => `${r.role}|${r.name}|${r.form || ""}`;
const stats = (xs) => ({
  n: xs.length,
  min: +Math.min(...xs).toFixed(1),
  max: +Math.max(...xs).toFixed(1),
  mean: +(xs.reduce((a, b) => a + b, 0) / xs.length).toFixed(1),
});

function dump(name, rows) {
  const f = path.join(DUMP, name + ".json");
  fs.writeFileSync(f, JSON.stringify(rows, null, 1));
  return f;
}

// ------------------------------------------------------------------ 1 identity
test("A1 identity: same ids over 3 walks with sibling insert/remove + attribute churn; gone; stale; never reused", async (t) => {
  const o = await open("register.html", { base: 500 });
  await o.ev(() => globalThis.__sb.walk({ base: 500 }));
  const first = await o.ev(() => globalThis.__sb.index());
  assert.ok(first.length > 15);
  assert.ok(first.every((r) => r.id >= 500), "ids start at base");
  const firstById = new Map(first.map((r) => [r.id, sig(r)]));
  const ever = new Set(first.map((r) => r.id));
  const tgt = await o.ev(() => {
    const sb = globalThis.__sb;
    return {
      email: sb.id(document.getElementById("email")),
      pw: sb.id(document.querySelector('input[name="password"]')),
      reg: sb.id(document.querySelector('#signup button[type="submit"]')),
      nl: sb.id(document.getElementById("nl-email")),
    };
  });
  const same = async (label) => {
    const rows = await o.ev(() => globalThis.__sb.index());
    const m = new Map(rows.map((r) => [r.id, sig(r)]));
    for (const [id, s] of firstById) assert.equal(m.get(id), s, `${label}: row #${id} ${s}`);
    assert.equal(new Set(rows.map((r) => r.id)).size, rows.length, `${label}: ids unique`);
    for (const r of rows) ever.add(r.id);
    const em = rows.find((r) => r.id === tgt.email);
    assert.equal(em && em.name, "Email");
    assert.equal(rows.find((r) => r.id === tgt.reg).name, "Register");
    return rows;
  };

  // walk 1 (dirty): siblings inserted before #email, a sibling removed, attribute churn
  await o.page.evaluate(() => {
    const lab = document.querySelector('label[for="email"]');
    const p = document.createElement("p");
    p.textContent = "Inserted note";
    const d = document.createElement("div");
    d.innerHTML = "<span>Inserted span</span>";
    const b = document.createElement("button");
    b.type = "button";
    b.textContent = "Inserted A";
    lab.before(p, d, b);
    document.querySelector("main > p").remove();
    const em = document.getElementById("email");
    for (let i = 0; i < 30; i++) {
      em.classList.toggle("c" + (i % 3));
      em.setAttribute("data-churn", String(i));
      document.querySelector("h1").style.letterSpacing = i + "px";
      document.getElementById("signup").setAttribute("data-n", String(i));
    }
  });
  await o.ev(() => globalThis.__sb.walk({ dirty: true }));
  await same("walk 1");

  // walk 2 (dirty): inserted siblings removed, 5 new buttons at the form start, churn on every
  // input, nav moved out and back (same element)
  await o.page.evaluate(() => {
    for (const el of document.querySelectorAll("#signup > p, #signup > div:not(.g-recaptcha), #signup > button[type=button]")) el.remove();
    const f = document.getElementById("signup");
    for (let i = 0; i < 5; i++) {
      const b = document.createElement("button");
      b.type = "button";
      b.textContent = "Extra " + i;
      f.prepend(b);
    }
    for (const el of document.querySelectorAll("input, select, button")) el.setAttribute("data-w2", "x");
    const nav = document.querySelector("nav");
    const parent = nav.parentNode;
    document.body.append(nav);
    parent.append(nav);
  });
  await o.ev(() => globalThis.__sb.walk({ dirty: true }));
  await same("walk 2");

  // walk 3 (full): more churn then a full re-walk
  await o.page.evaluate(() => {
    for (const el of document.querySelectorAll("*")) el.setAttribute("data-w3", "1");
    document.querySelector("footer p").before(document.createElement("hr"));
  });
  await o.ev(() => globalThis.__sb.walk({}));
  await same("walk 3");

  // removed element -> gone (resolve and act)
  await o.page.evaluate(() => {
    window.__removed = document.getElementById("email");
    window.__removed.remove();
  });
  assert.deepEqual(await o.ev((i) => {
    const r = globalThis.__sb.resolve(i);
    return r && r.nodeType ? "element" : r;
  }, tgt.email), { why: "gone" });
  assert.deepEqual((await timedAct(o, tgt.email)).r, { why: "gone" });
  assert.ok(!(await o.ev(() => globalThis.__sb.index())).some((r) => r.id === tgt.email));

  // below base -> stale (number and #ref string)
  for (const id of [499, 1, "#499"]) {
    assert.deepEqual(await o.ev((i) => globalThis.__sb.resolve(i), id), { why: "stale" }, String(id));
  }
  assert.deepEqual((await timedAct(o, 499)).r, { why: "stale" });
  // above every issued id -> gone (never issued)
  assert.deepEqual(await o.ev((i) => globalThis.__sb.resolve(i), 99999), { why: "gone" });

  // re-add the same node: same element, same id
  await o.page.evaluate(() => document.querySelector('label[for="email"]').after(window.__removed));
  assert.equal(await o.ev((i) => globalThis.__sb.resolve(i) === document.getElementById("email"), tgt.email), true);
  await o.ev(() => globalThis.__sb.walk({ dirty: true }));
  await same("after re-add");

  // remove rows, drop every reference, collect garbage, full walk, then add fresh elements:
  // their ids are new, above everything issued before
  const maxBefore = Math.max(...ever);
  await o.page.evaluate(() => {
    window.__removed = null;
    for (const b of document.querySelectorAll("#signup > button[type=button]")) b.remove();
  });
  await o.cdp.send("HeapProfiler.collectGarbage");
  await o.ev(() => globalThis.__sb.walk({}));
  await o.page.evaluate(() => {
    const f = document.getElementById("signup");
    f.prepend(document.getElementById("email").cloneNode(true));
    for (let i = 0; i < 5; i++) {
      const b = document.createElement("button");
      b.type = "button";
      b.textContent = "Extra " + i; // same names as the removed rows
      f.prepend(b);
    }
  });
  await o.ev(() => globalThis.__sb.walk({ dirty: true }));
  const rows = await o.ev(() => globalThis.__sb.index());
  const fresh = rows.filter((r) => !ever.has(r.id));
  assert.equal(fresh.length, 6, JSON.stringify(fresh.map(sig)));
  for (const r of fresh) assert.ok(r.id > maxBefore, `fresh ${sig(r)} id ${r.id} > ${maxBefore}`);
  t.diagnostic(`ACCEPT A1 rows=${first.length} ids ${Math.min(...first.map((r) => r.id))}..${maxBefore} fresh after GC ${fresh.map((r) => r.id).join(",")}`);
});

// ------------------------------------------------------------------ 2 actionability
test("A2 act in page: covered < 50 ms; disabled; hidden; moving; centre after the cover is removed", async (t) => {
  const o = await open("overlay.html");
  await o.ev(() => globalThis.__sb.walk({ base: 1 }));
  const rows = fill(await o.ev(() => globalThis.__sb.index()));
  const buy = byName(rows, "Buy now");
  const later = byName(rows, "Pay later");
  const mover = byName(rows, "Moving");
  const gone = byName(rows, "Gone soon");
  const cookie = rows.find((r) => r.flags.overlay);
  assert.ok(buy && later && mover && gone && cookie);

  const cov = [];
  for (let i = 0; i < 10; i++) {
    const c = await timedAct(o, buy.id);
    assert.equal(c.r.why, `covered by div.cookie (OVERLAY #${cookie.id})`);
    cov.push(c.ms);
  }
  const cs = stats(cov);
  t.diagnostic(`ACCEPT A2 covered ms ${JSON.stringify(cs)} why="covered by div.cookie (OVERLAY #${cookie.id})"`);
  assert.ok(cs.max < 50, `covered max ${cs.max} ms`);

  const dis = [];
  for (let i = 0; i < 5; i++) {
    const d = await timedAct(o, later.id);
    assert.deepEqual(d.r, { why: "disabled" });
    dis.push(d.ms);
  }
  t.diagnostic(`ACCEPT A2 disabled ms ${JSON.stringify(stats(dis))}`);

  // hidden: an indexed row hidden later, fresh display:none / visibility:hidden / zero-size
  await o.page.evaluate(() => {
    document.getElementById("gone").style.display = "none";
    const m = document.querySelector("main");
    m.insertAdjacentHTML("beforeend",
      '<button id="hn" style="display:none">H none</button>' +
      '<button id="hv" style="visibility:hidden">H vis</button>' +
      '<button id="hz" style="width:0;height:0;padding:0;border:0;overflow:hidden">H zero</button>');
  });
  const hid = [];
  const hres = await o.ev(async (gid) => {
    const sb = globalThis.__sb;
    const out = [];
    for (const id of [gid, ...["hn", "hv", "hz"].map((x) => sb.id(document.getElementById(x)))]) {
      const t0 = performance.now();
      out.push({ r: await sb.act(id), ms: performance.now() - t0 });
    }
    return out;
  }, gone.id);
  for (const h of hres) {
    assert.deepEqual(h.r, { why: "hidden" });
    hid.push(h.ms);
  }
  t.diagnostic(`ACCEPT A2 hidden ms ${JSON.stringify(stats(hid))}`);

  const mov = [];
  for (let i = 0; i < 5; i++) {
    const m = await timedAct(o, mover.id);
    assert.deepEqual(m.r, { why: "not stable (moving)" });
    mov.push(m.ms);
  }
  t.diagnostic(`ACCEPT A2 moving ms ${JSON.stringify(stats(mov))}`);

  // remove the cover: act succeeds with {x, y} at the centre of Buy now
  await o.page.evaluate(() => document.querySelector(".cookie").remove());
  const ok = await timedAct(o, buy.id);
  assert.equal(typeof ok.r.x, "number", JSON.stringify(ok.r));
  const c = await o.page.evaluate(([x, y]) => {
    const r = document.getElementById("buy").getBoundingClientRect();
    return { cx: r.left + r.width / 2, cy: r.top + r.height / 2, hit: document.elementFromPoint(x, y).id };
  }, [ok.r.x, ok.r.y]);
  assert.ok(Math.abs(ok.r.x - c.cx) <= 1 && Math.abs(ok.r.y - c.cy) <= 1, JSON.stringify({ r: ok.r, c }));
  assert.equal(c.hit, "buy");
  await o.page.mouse.click(ok.r.x, ok.r.y);
  assert.equal(await o.page.textContent("#log"), "bought");
  t.diagnostic(`ACCEPT A2 after cover removed: ${JSON.stringify(ok.r)} centre (${c.cx.toFixed(1)}, ${c.cy.toFixed(1)}) in ${ok.ms.toFixed(1)} ms`);

  // the infinite mover stops (animation paused): stable, centre
  await o.page.evaluate(() => document.getElementById("mover").getAnimations().forEach((a) => a.pause()));
  const st = await timedAct(o, mover.id);
  assert.equal(typeof st.r.x, "number", JSON.stringify(st.r));
  const mc = await o.page.evaluate(() => {
    const r = document.getElementById("mover").getBoundingClientRect();
    return { cx: r.left + r.width / 2, cy: r.top + r.height / 2 };
  });
  assert.ok(Math.abs(st.r.x - mc.cx) <= 1 && Math.abs(st.r.y - mc.cy) <= 1, JSON.stringify({ r: st.r, mc }));
});

test("A2b act on a finite slide: not stable while it moves, {x,y} at the final centre once it stops", async (t) => {
  const o = await open("accept-move.html");
  await o.ev(() => globalThis.__sb.walk({ base: 1 }));
  const slide = byName(fill(await o.ev(() => globalThis.__sb.index())), "Sliding");
  assert.ok(slide);
  await o.page.evaluate(() => window.__start());
  const seq = [];
  const t0 = Date.now();
  for (;;) {
    const a = await timedAct(o, slide.id);
    seq.push(a);
    if (typeof a.r.x === "number" || Date.now() - t0 > 4000) break;
  }
  const { startedAt, endedAt, cx, cy } = await o.page.evaluate(() => {
    const r = document.getElementById("slide").getBoundingClientRect();
    return { startedAt: window.__startedAt, endedAt: window.__endedAt, cx: r.left + r.width / 2, cy: r.top + r.height / 2 };
  });
  const last = seq[seq.length - 1];
  const moving = seq.slice(0, -1);
  assert.ok(moving.length >= 3, `saw ${moving.length} moving answers`);
  for (const m of moving) assert.deepEqual(m.r, { why: "not stable (moving)" });
  assert.equal(typeof last.r.x, "number", JSON.stringify(last.r));
  assert.ok(endedAt !== null, "transitionend fired");
  // success only once the slide has stopped (one frame of slack for transitionend dispatch)
  assert.ok(last.end >= endedAt - 20, `success at ${last.end.toFixed(0)} before end ${endedAt.toFixed(0)}`);
  assert.ok(Math.abs(last.r.x - cx) <= 1 && Math.abs(last.r.y - cy) <= 1, JSON.stringify({ r: last.r, cx, cy }));
  t.diagnostic(`ACCEPT A2b ${moving.length} x "not stable (moving)" then ${JSON.stringify(last.r)}; ` +
    `transition ${(endedAt - startedAt).toFixed(0)} ms, success ${(last.end - endedAt).toFixed(0)} ms after transitionend; ` +
    `moving answers ms ${JSON.stringify(stats(moving.map((m) => m.ms)))}`);
});

// ------------------------------------------------------------------ 3 settle
test("A3a settle: spa.html renders 400 ms after load; index non-empty after settle", async (t) => {
  const o = await open("spa.html");
  assert.equal((await o.ev(() => globalThis.__sb.index())).length, 0);
  const s = await timedSettle(o);
  assert.equal(s.r.settled, true);
  const rows = fill(await o.ev(() => globalThis.__sb.index()));
  assert.ok(rows.length >= 3, JSON.stringify(rows.map(sig)));
  assert.ok(byName(rows, "Dashboard", "heading") && byName(rows, "Search projects", "textbox"));
  assert.ok(s.ms < 3000);
  t.diagnostic(`ACCEPT A3a spa settle ${s.ms.toFixed(0)} ms changed=${s.r.changed} rows=${rows.length}`);
});

test("A3b settle: cap never exceeded over 20 runs on busy.html", async (t) => {
  const o = await open("busy.html");
  const ms = [];
  for (let i = 0; i < 20; i++) {
    const s = await timedSettle(o);
    assert.equal(s.r.settled, false);
    assert.ok(s.r.changed > 10);
    ms.push(s.ms);
  }
  const st = stats(ms);
  t.diagnostic(`ACCEPT A3b busy default cap x20 ms ${JSON.stringify(st)} overrun max ${(st.max - 3000).toFixed(1)} ms`);
  // the cap is a setTimeout(3000); allow timer latency, never a second quiet window
  assert.ok(st.max < 3050, `max ${st.max} ms`);
  assert.ok(st.min >= 2990, `min ${st.min} ms`);
});

test("A3c settle on a blank page (empty body, about:blank): duration reported", async (t) => {
  const o = await open("accept-blank.html");
  const s = await timedSettle(o);
  const flag = !s.r.settled && s.ms >= 2900 ? " FLAG burns the full cap" : "";
  t.diagnostic(`ACCEPT A3c empty-body settle ${s.ms.toFixed(0)} ms settled=${s.r.settled}${flag}`);
  assert.ok(s.ms < 3050, `cap exceeded: ${s.ms} ms`);
  // D-405 (8): no scripts and an empty body means no content wait
  assert.ok(s.ms < 1000, `empty-body settle took ${s.ms.toFixed(0)} ms (D-405 (8): well under 1 s)`);
  assert.equal(s.r.settled, true);
  const q = await timedSettle(o, { cap: 500 });
  t.diagnostic(`ACCEPT A3c empty-body settle cap 500: ${q.ms.toFixed(0)} ms settled=${q.r.settled}`);
  // about:blank: is there an sb world at all?
  const b = await open("about:blank");
  let msg;
  let ms = null;
  try {
    const sb = await timedSettle(b);
    ms = sb.ms;
    const flag2 = !sb.r.settled && sb.ms >= 2900 ? " FLAG burns the full cap" : "";
    msg = `about:blank settle ${sb.ms.toFixed(0)} ms settled=${sb.r.settled}${flag2}`;
  } catch (e) {
    msg = `about:blank: ${String(e.message).slice(0, 120)}`;
  }
  t.diagnostic(`ACCEPT A3c ${msg}`);
  assert.ok(ms !== null, msg);
  assert.ok(ms < 1000, `${msg} (D-405 (8): well under 1 s)`);
});

// ------------------------------------------------------------------ 4 register index + wire shape
test("A4 register.html index rows (acceptance 6 index side) and I-5 wire shape of dumped index() JSON", async (t) => {
  const o = await open("register.html");
  await o.ev(() => globalThis.__sb.walk({ base: 1 }));
  const raw = await o.ev(() => globalThis.__sb.index());
  const files = [dump("register", raw)];
  const rows = fill(raw);

  const emails = rows.filter((r) => r.name === "Email");
  assert.equal(emails.length, 2);
  const [em, nl] = emails;
  assert.equal(em.role, "textbox");
  assert.equal(em.form, "f1");
  assert.equal(em.state.input_type, "email");
  assert.equal(nl.form, "f2");
  assert.ok(em.id < nl.id);

  const pw = byName(rows, "Password");
  assert.equal(pw.role, "textbox");
  assert.equal(pw.form, "f1");
  assert.deepEqual(pw.state.value, { kind: "empty" });
  assert.equal(pw.state.input_type, "password");
  assert.equal(pw.state.autocomplete, "new-password");

  const fn = byName(rows, "Full name", "textbox");
  assert.ok(fn && fn.form === "f1");
  const plan = byName(rows, "Plan", "combobox");
  assert.deepEqual(plan.state.options, ["Basic", "Plus", "Pro"]);
  assert.equal(plan.form, "f1");
  const agree = byName(rows, "I agree to the Terms of Service", "checkbox");
  assert.equal(agree.state.checked, false);
  assert.equal(agree.form, "f1");

  const ch = rows.filter((r) => r.flags.challenge || /recaptcha/i.test(r.name));
  assert.equal(ch.length, 1, JSON.stringify(ch.map(sig)));
  assert.equal(ch[0].role, "iframe");
  assert.ok(ch[0].flags.challenge && ch[0].flags.opaque);

  const reg = byName(rows, "Register", "button");
  assert.ok(reg.flags.submit && reg.form === "f1");

  const ov = rows.filter((r) => r.flags.overlay);
  assert.equal(ov.length, 1);
  assert.equal(ov[0].name, "Cookie consent");

  // hidden promo: none of its rows
  assert.ok(!rows.some((r) => /offer|promo/i.test(r.name)), JSON.stringify(rows.map(sig)));
  assert.equal(rows.filter((r) => r.role === "button" && /^Register/.test(r.name)).length, 1);
  // document order of the acceptance-6 rows
  const order = [em, pw, agree, ch[0], reg, ov[0]].map((r) => r.id);
  assert.deepEqual(order, [...order].sort((a, b) => a - b));

  // text(): the wrapping label's text is not emitted again; the hidden promo's text is absent
  const text = await o.ev(() => globalThis.__sb.text());
  const all = text.map((b) => b.spans.map((s) => s.text).join("")).join("\n");
  assert.ok(!/I agree/.test(all), "agree label text re-emitted:\n" + all);
  assert.ok(!/Limited offer|50% off/.test(all), "promo text emitted");
  assert.ok(/Start your free trial/.test(all));

  // more dumps for the shape checker: live values, other fixtures
  await o.page.fill("#email", "ada@example.org");
  await o.page.fill('input[name="password"]', "pw-for-shape-only-1");
  await o.page.check('input[name="agree"]');
  await o.page.selectOption("#plan", "pro");
  const typed = await o.ev(() => globalThis.__sb.index());
  files.push(dump("register-typed", typed));
  const tr = fill(typed);
  assert.deepEqual(byName(tr, "Password").state.value, { kind: "filled" });
  assert.deepEqual(tr.find((r) => r.id === em.id).state.value, { kind: "text", text: "ada@example.org" });
  for (const [name, url] of [["overlay", "overlay.html"], ["frames", "frames.html?xo=" + encodeURIComponent(B.origin)],
    ["controls", "controls.html"], ["popup", "popup.html"], ["dialog", "dialog.html"], ["spa", "spa.html"]]) {
    const p = await open(url);
    if (name === "spa") await p.ev(() => globalThis.__sb.settle());
    files.push(dump(name, await p.ev(() => globalThis.__sb.index())));
  }
  const res = spawnSync(process.execPath, [path.join(__dirname, "check-wire-shape.js"), ...files], { encoding: "utf8" });
  t.diagnostic("ACCEPT A4 shape: " + res.stdout.trim().split("\n").join(" | "));
  assert.equal(res.status, 0, res.stdout + res.stderr);
});

// ------------------------------------------------------------------ 5 password never read
test("A5 password typed via Playwright never appears in any __sb output; value getter never called", async (t) => {
  const secret = process.env.SB_VAULT_CANARY || "sbpw" + crypto.randomBytes(12).toString("hex");
  const o = await open("register.html");
  await o.ev(() => globalThis.__sb.walk({ base: 1 }));
  // trap the value getter in world sb (the world injected.js runs in)
  await o.ev(() => {
    const d = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value");
    globalThis.__pwReads = 0;
    Object.defineProperty(HTMLInputElement.prototype, "value", {
      configurable: true,
      enumerable: d.enumerable,
      get() {
        if (String(this.type).toLowerCase() === "password" || this.name === "password") globalThis.__pwReads++;
        return d.get.call(this);
      },
      set(v) {
        d.set.call(this, v);
      },
    });
  });
  await o.page.fill('input[name="password"]', secret.slice(0, 10));
  await o.page.locator('input[name="password"]').pressSequentially(secret.slice(10));
  await o.page.fill("#email", "ada@example.org");
  const out = await o.ev(async () => {
    const sb = globalThis.__sb;
    const pw = document.querySelector('input[name="password"]');
    const r = {};
    r.walkDirty = sb.walk({ dirty: true });
    r.index = sb.index();
    r.walkFull = sb.walk({});
    r.index2 = sb.index();
    r.text = sb.text();
    r.textF1 = sb.text("f1");
    r.textMain = sb.text("main");
    r.fp1 = sb.fingerprint('input "Password" button "Register" SUBMIT form#f1');
    r.fp2 = sb.fingerprint('textbox "Password" checkbox "nothing like this"');
    r.frames = sb.frames();
    r.id = sb.id(pw);
    r.name = sb.name(pw);
    r.role = sb.role(pw);
    r.state = sb.state(pw);
    r.act = await sb.act(r.id);
    r.settle = await sb.settle({ cap: 400 });
    r.reads = globalThis.__pwReads;
    return r;
  });
  const blob = JSON.stringify(out);
  const pwRow = fill(out.index2).find((x) => x.id === out.id);
  assert.deepEqual(pwRow.state.value, { kind: "filled" });
  assert.deepEqual(out.state.value, { kind: "filled" });
  assert.equal(out.reads, 0, "injected.js called the password value getter");
  assert.equal(blob.includes(secret), false, "secret found in __sb output");
  for (let i = 0; i + 8 <= secret.length; i += 4) {
    assert.equal(blob.includes(secret.slice(i, i + 8)), false, "a fragment of the secret found in __sb output");
  }
  assert.ok(blob.includes("ada@example.org"), "control: a non-sensitive value is read");
  // the page really holds it (read from the main world, not sb)
  assert.equal(await o.page.inputValue('input[name="password"]'), secret);
  t.diagnostic(`ACCEPT A5 secret len ${secret.length} absent from ${blob.length} bytes of walk/index/text/fingerprint/frames/state/name/act/settle output; sb-world value-getter reads=${out.reads}`);
});

test("A5b show-password toggle (type password -> text) still never emits the value", async (t) => {
  const secret = "sbpw" + crypto.randomBytes(12).toString("hex");
  const o = await open("register.html");
  await o.page.fill('input[name="password"]', secret);
  await o.page.evaluate(() => { document.querySelector('input[name="password"]').type = "text"; });
  const out = await o.ev(() => {
    const sb = globalThis.__sb;
    sb.walk({ dirty: true });
    return { index: sb.index(), text: sb.text(), state: sb.state(document.querySelector('input[name="password"]')) };
  });
  const blob = JSON.stringify(out);
  t.diagnostic(`ACCEPT A5b toggled field state ${JSON.stringify(out.state)}`);
  assert.equal(blob.includes(secret), false, "secret emitted after type=text toggle");
  assert.deepEqual(out.state.value, { kind: "filled" });
});

// ------------------------------------------------------------------ 6 isolation
// Same as helpers.openSb but with a main-world init script that runs before any page script.
async function openProbe(url, mainInit) {
  const context = await browser.newContext({ viewport: { width: 1280, height: 720 } });
  if (mainInit) await context.addInitScript(mainInit);
  const pg = await context.newPage();
  const cdp = await context.newCDPSession(pg);
  const worlds = new Map();
  cdp.on("Runtime.executionContextCreated", ({ context: c }) => {
    if (c.name === "sb" && c.auxData && c.auxData.frameId) worlds.set(c.auxData.frameId, c.id);
  });
  await cdp.send("Runtime.enable");
  await cdp.send("Page.enable");
  await cdp.send("Page.addScriptToEvaluateOnNewDocument", { source: SRC, worldName: "sb" });
  await cdp.send("Page.addScriptToEvaluateOnNewDocument", { source: MEDIA_SRC, worldName: "sb" });
  await pg.goto(url, { waitUntil: "load" });
  const mainId = (await cdp.send("Page.getFrameTree")).frameTree.frame.id;
  const ev = async (fn, arg) => {
    const r = await cdp.send("Runtime.evaluate", {
      expression: `(${fn})(${JSON.stringify(arg === undefined ? null : arg)})`,
      contextId: worlds.get(mainId), returnByValue: true, awaitPromise: true,
    });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception ? r.exceptionDetails.exception.description : r.exceptionDetails.text);
    return r.result.value;
  };
  const o = { context, page: pg, cdp, ev, close: () => context.close() };
  opened.push(o);
  return o;
}

const PROBE = `(() => {
  const counts = {};
  const bump = (k) => { counts[k] = (counts[k] || 0) + 1; };
  const wrap = (obj, key, label) => {
    const d = Object.getOwnPropertyDescriptor(obj, key);
    if (!d) return;
    if (typeof d.value === "function") {
      const orig = d.value;
      Object.defineProperty(obj, key, Object.assign({}, d, { value: function (...a) { bump(label); return orig.apply(this, a); } }));
    } else if (d.get) {
      const g = d.get;
      Object.defineProperty(obj, key, Object.assign({}, d, { get: function () { bump(label); return g.call(this); } }));
    }
  };
  const NativeMO = window.MutationObserver;
  const rec = [];
  const mo = new NativeMO((ms) => { for (const m of ms) rec.push(m.type + ":" + (m.attributeName || "") + ":" + (m.target.nodeName || "")); });
  mo.observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
  window.MutationObserver = class extends NativeMO { constructor(cb) { bump("new MutationObserver"); super(cb); } };
  wrap(NativeMO.prototype, "observe", "MutationObserver.observe");
  wrap(EventTarget.prototype, "addEventListener", "addEventListener");
  wrap(Element.prototype, "getBoundingClientRect", "getBoundingClientRect");
  wrap(Element.prototype, "getClientRects", "getClientRects");
  wrap(Element.prototype, "scrollIntoView", "scrollIntoView");
  wrap(Element.prototype, "scrollIntoViewIfNeeded", "scrollIntoViewIfNeeded");
  wrap(Element.prototype, "checkVisibility", "checkVisibility");
  wrap(Element.prototype, "matches", "matches");
  wrap(Element.prototype, "closest", "closest");
  wrap(Element.prototype, "getAttribute", "getAttribute");
  wrap(Element.prototype, "getAnimations", "Element.getAnimations");
  wrap(Document.prototype, "getAnimations", "Document.getAnimations");
  wrap(Document.prototype, "createTreeWalker", "createTreeWalker");
  wrap(Document.prototype, "elementFromPoint", "elementFromPoint");
  wrap(Document.prototype, "elementsFromPoint", "elementsFromPoint");
  wrap(Document.prototype, "querySelectorAll", "Document.querySelectorAll");
  wrap(Element.prototype, "querySelectorAll", "Element.querySelectorAll");
  wrap(Node.prototype, "textContent", "textContent");
  wrap(HTMLElement.prototype, "innerText", "innerText");
  wrap(HTMLInputElement.prototype, "value", "input.value");
  wrap(window, "getComputedStyle", "getComputedStyle");
  wrap(window, "requestAnimationFrame", "requestAnimationFrame");
  wrap(window, "setTimeout", "setTimeout");
  wrap(window, "WeakRef", "WeakRef");
  wrap(Object, "defineProperty", "Object.defineProperty");
  wrap(Object, "freeze", "Object.freeze");
  Object.defineProperty(window, "__probe", { value: () => ({ counts: Object.assign({}, counts), muts: rec.slice() }), enumerable: false });
})();`;

test("A6 isolation: no __sb / __sbMedia in the main world; site cannot see sb's MutationObserver, patches, DOM writes or globals", async (t) => {
  const o = await openProbe(A.origin + "/register.html", PROBE);
  // main world view
  const mw = await o.page.evaluate(() => ({
    sb: typeof window.__sb,
    media: typeof window.__sbMedia,
    inSb: "__sb" in window,
    inMedia: "__sbMedia" in window,
    sbKeys: Object.getOwnPropertyNames(window).filter((k) => /^__sb/i.test(k)),
  }));
  assert.deepEqual(mw, { sb: "undefined", media: "undefined", inSb: false, inMedia: false, sbKeys: [] });
  // world sb has both
  assert.equal(await o.ev(() => typeof globalThis.__sb.walk + typeof globalThis.__sbMedia.list), "functionfunction");

  const html0 = await o.page.evaluate(() => document.documentElement.outerHTML);
  const p0 = await o.page.evaluate(() => window.__probe());
  // a full sb workload
  const work = await o.ev(async () => {
    const sb = globalThis.__sb;
    sb.walk({ base: 1 });
    const rows = sb.index();
    sb.text();
    sb.frames();
    sb.fingerprint('button "Register" SUBMIT');
    for (const el of document.querySelectorAll("input, button, select")) {
      sb.name(el);
      sb.role(el);
      sb.state(el);
    }
    const reg = rows.find((r) => r.name === "Register");
    const em = rows.find((r) => r.name === "Email");
    const a1 = await sb.act(reg.id);
    const a2 = await sb.act(em.id);
    const s = await sb.settle({ cap: 600 });
    sb.walk({ dirty: true });
    globalThis.__sbMedia.list();
    return { rows: rows.length, a1, a2, s };
  });
  const p1 = await o.page.evaluate(() => window.__probe());
  const html1 = await o.page.evaluate(() => document.documentElement.outerHTML);
  const delta = {};
  for (const k of new Set([...Object.keys(p0.counts), ...Object.keys(p1.counts)])) {
    const d = (p1.counts[k] || 0) - (p0.counts[k] || 0);
    if (d) delta[k] = d;
  }
  t.diagnostic(`ACCEPT A6 sb workload ${JSON.stringify(work)}; main-world probe call deltas ${JSON.stringify(delta)}; ` +
    `main-world MutationObserver records during workload ${p1.muts.length - p0.muts.length}; probe constructions total ${p1.counts["new MutationObserver"] || 0}`);
  assert.ok(work.rows > 10);
  assert.deepEqual(delta, {}, "main-world patched functions were called by world sb");
  assert.equal(p1.counts["new MutationObserver"] || 0, 0, "a MutationObserver was constructed through the main-world constructor");
  assert.deepEqual(p1.muts.slice(p0.muts.length), [], "world sb mutated the DOM");
  assert.equal(html1, html0, "outerHTML changed");
  // positive control: the probe does see main-world calls and DOM writes
  const ctl = await o.page.evaluate(async () => {
    const c0 = window.__probe();
    document.body.getBoundingClientRect();
    document.body.setAttribute("data-ctl", "1");
    await Promise.resolve();
    const c1 = window.__probe();
    document.body.removeAttribute("data-ctl");
    return { gbcr: (c1.counts.getBoundingClientRect || 0) - (c0.counts.getBoundingClientRect || 0), muts: c1.muts.length - c0.muts.length };
  });
  assert.deepEqual(ctl, { gbcr: 1, muts: 1 }, "probe positive control");

  // main-world natives untouched; no expandos on document or elements
  const nat = await o.page.evaluate(() => {
    // functions the probe did not wrap itself
    const fns = [history.pushState, history.replaceState, window.open, window.fetch, XMLHttpRequest.prototype.open,
      Element.prototype.attachShadow, HTMLElement.prototype.click, Function.prototype.toString];
    const notNative = fns.filter((f) => !/\[native code\]/.test(Function.prototype.toString.call(f))).map((f) => f.name);
    // own names a site could see on nodes, minus the platform's own (form/select indices,
    // document.location, which a plain page without sb has too)
    const expando = [document, ...document.querySelectorAll("*")].filter((n) =>
      Object.getOwnPropertyNames(n).some((k) => !/^\d+$/.test(k) && !(n === document && k === "location"))).length;
    return { notNative, expando };
  });
  assert.deepEqual(nat.notNative, []);
  assert.equal(nat.expando, 0);

  // window globals: a page with sb installed exposes exactly the same names as one without
  const plainCtx = await browser.newContext();
  try {
    const plain = await plainCtx.newPage();
    await plain.goto(A.origin + "/register.html");
    const sbo = await openSb(browser, A.origin + "/register.html", { media: true });
    opened.push(sbo);
    await sbo.ev(() => { globalThis.__sb.walk({ base: 1 }); globalThis.__sb.index(); });
    const names = (p) => p.evaluate(() => Object.getOwnPropertyNames(window).sort());
    const [a, b] = await Promise.all([names(plain), names(sbo.page)]);
    assert.deepEqual(b.filter((k) => !a.includes(k)), [], "extra main-world globals with sb installed");
    t.diagnostic(`ACCEPT A6 main-world window names: plain ${a.length}, with sb ${b.length}`);
  } finally {
    await plainCtx.close();
  }
});

// ------------------------------------------------------------------ 7 fixtures
test("A7 fixtures: popup, dialog, spa, overlay, frames present and exercised", async (t) => {
  const notes = [];
  // popup.html
  {
    const o = await open("popup.html");
    const rows = fill(await o.ev(() => globalThis.__sb.index()));
    assert.equal(byName(rows, "Open register in a new tab", "link").state.opens_new_tab, true);
    assert.equal(byName(rows, "Open popup window", "button").state.opens_new_tab, true);
    assert.equal(byName(rows, "Register in this tab", "link").state.opens_new_tab, false);
    assert.equal(byName(rows, "Does nothing", "button").state.opens_new_tab, false);
    notes.push("popup ok");
  }
  // dialog.html: rows and an act point for each control
  {
    const o = await open("dialog.html");
    const rows = fill(await o.ev(() => globalThis.__sb.index()));
    for (const [n, role] of [["Show alert", "button"], ["Delete item", "button"], ["Ask name", "button"], ["Download notes", "link"]]) {
      const r = byName(rows, n, role);
      assert.ok(r, n);
      const a = await timedAct(o, r.id);
      assert.equal(typeof a.r.x, "number", n + " " + JSON.stringify(a.r));
    }
    notes.push("dialog ok");
  }
  // spa.html
  {
    const o = await open("spa.html");
    await o.ev(() => globalThis.__sb.settle());
    assert.ok(byName(fill(await o.ev(() => globalThis.__sb.index())), "Search", "button"));
    notes.push("spa ok");
  }
  // overlay.html
  {
    const o = await open("overlay.html");
    const rows = fill(await o.ev(() => globalThis.__sb.index()));
    assert.equal(rows.filter((r) => r.flags.overlay).length, 1);
    assert.equal(byName(rows, "Buy now").state.covered, true);
    notes.push("overlay ok");
  }
  // frames.html with a real cross-origin child, and the sandboxed fallback
  for (const q of ["?xo=" + encodeURIComponent(B.origin), ""]) {
    const o = await open("frames.html" + q);
    const rows = fill(await o.ev(() => globalThis.__sb.index()));
    const same = byName(rows, "Same-origin child", "iframe");
    const cross = byName(rows, "Cross-origin child", "iframe");
    const cap = rows.find((r) => r.flags.challenge);
    assert.ok(same && cross && cap);
    assert.equal(same.flags.opaque, false);
    assert.equal(cross.flags.opaque, true);
    assert.ok(cap.flags.opaque && cap.role === "iframe");
    const list = await o.ev(() => globalThis.__sb.frames());
    assert.equal(list[0].frame, 0);
    assert.equal(list[0].parent, null);
    const kids = list.slice(1);
    assert.deepEqual(kids.map((f) => f.id), [same.id, cross.id, cap.id]);
    assert.deepEqual(kids.map((f) => f.opaque), [false, true, true]);
    assert.ok(kids.every((f) => f.parent === 0));
    assert.equal(kids[0].url, A.origin + "/frame-child.html");
    if (q) assert.equal(kids[1].url, B.origin + "/frame-child.html");
    else assert.equal(kids[1].url, "about:srcdoc");
    // the same-origin child has its own __sb with its own rows
    const tree = await o.frames();
    const sf = tree.find((f) => f.url === A.origin + "/frame-child.html");
    // each frame gets its own first based walk from sb-driver (D-405 (2))
    const childRows = await o.ev(() => {
      globalThis.__sb.walk({ base: 1 });
      return globalThis.__sb.index();
    }, undefined, sf.id);
    assert.ok(byName(fill(childRows), "Nickname", "textbox"));
    notes.push(`frames${q ? " xo" : " sandboxed"} ok (${kids.map((f) => (f.opaque ? "OPAQUE " : "") + f.url.replace(/^http:\/\/127\.0\.0\.1:\d+/, "")).join(", ")})`);
  }
  t.diagnostic("ACCEPT A7 " + notes.join("; "));
});

// ================================================================== round 2 (tester-7)
// Pins what D-405, D-425, D-426, D-429 and D-73 (I-11) promise and no test pinned in Chromium.

const NB = { why: "no base" };
const flat = (blocks) => blocks.map((b) => b.spans.map((s) => s.text).join("")).join(" / ");

// ------------------------------------------------------------------ A8 no-base shapes
test("A8 no base (D-73, D-425): every reading call answers exactly {why:\"no base\"} before walk({base}); role/resolve/act/settle work; nothing issued", async (t) => {
  const o = await open("register.html", { base: null, media: true });
  const out = await o.ev(async () => {
    const sb = globalThis.__sb;
    const m = globalThis.__sbMedia;
    const btn = Array.from(document.querySelectorAll("button")).find((b) => b.textContent === "Register");
    const pw = document.querySelector('input[name="password"]');
    const r = {};
    r.id = sb.id(btn);
    r.idPw = sb.id(pw);
    r.idHtml = sb.id(document.documentElement);
    r.index = sb.index();
    r.text = sb.text();
    r.textF1 = sb.text("f1");
    r.textMain = sb.text("main");
    r.fp = sb.fingerprint('button "Register" SUBMIT');
    r.fpEmpty = sb.fingerprint("");
    r.state = sb.state(btn);
    r.statePw = sb.state(pw);
    r.name = sb.name(btn);
    r.frames = sb.frames();
    const walks = [undefined, null, true, false, {}, { dirty: true }, { dirty: [1, 2] }, { base: "5" }, { base: "#5" },
      { base: 0 }, { base: -1 }, { base: 1.5 }, { base: NaN }, { base: Infinity }, { base: 2 ** 53 }, { base: null },
      { base: true }, { base: [1] }, { base: {} }];
    const show = (w) => (w === undefined ? "undefined" : JSON.stringify(w, (k, v) => (typeof v === "number" && !Number.isFinite(v) ? String(v) : v)));
    r.walks = walks.map((w) => ({ arg: show(w), got: sb.walk(w) }));
    r.walkNoArg = sb.walk();
    r.role = sb.role(btn);
    r.resolve1 = sb.resolve(1);
    r.resolveRef = sb.resolve("#1");
    r.resolveBig = sb.resolve(99999);
    r.act = await sb.act(1);
    r.settle = await sb.settle({ cap: 400 });
    r.mList = m.list();
    r.mCues = m.cues(1);
    r.mFrame = await m.frame(1, 0.1);
    r.mFrames = await m.frames(1, [0.1, 0.2]);
    // then the first based walk: everything issued is >= base, so nothing was issued before
    r.based = sb.walk({ base: 700 });
    r.idAfter = sb.id(btn);
    r.rows = sb.index();
    r.resolve699 = sb.resolve(699);
    r.later = sb.walk({ base: 5 });
    r.idLater = sb.id(btn);
    return r;
  });
  const keys = ["id", "idPw", "idHtml", "index", "text", "textF1", "textMain", "fp", "fpEmpty", "state", "statePw", "name",
    "frames", "walkNoArg", "mList", "mCues"];
  for (const k of keys) {
    assert.deepEqual(out[k], NB, k);
    assert.equal(JSON.stringify(out[k]), JSON.stringify(NB), k);
  }
  for (const w of out.walks) assert.deepEqual(w.got, NB, "walk(" + w.arg + ")");
  // media frame()/frames() use their entry shape for a whole-call refusal ({t: null, why}, as
  // for gone/challenge/DRM; I-11 promises a bare {why} only for list() and cues())
  assert.equal(out.mFrame.why, "no base", JSON.stringify(out.mFrame));
  assert.ok(!("hash" in out.mFrame) && !("data" in out.mFrame));
  assert.ok(Array.isArray(out.mFrames) && out.mFrames.length >= 1, JSON.stringify(out.mFrames));
  for (const e of out.mFrames) {
    assert.equal(e.why, "no base", JSON.stringify(out.mFrames));
    assert.ok(!("hash" in e) && !("data" in e));
  }
  assert.equal(out.role, "button");
  assert.deepEqual(out.resolve1, { why: "stale" });
  assert.deepEqual(out.resolveRef, { why: "stale" });
  assert.deepEqual(out.resolveBig, { why: "stale" });
  assert.deepEqual(out.act, { why: "stale" });
  assert.equal(typeof out.settle.settled, "boolean");
  assert.equal(typeof out.settle.changed, "number");
  assert.ok(out.based.count > 15, JSON.stringify(out.based));
  assert.ok(out.idAfter >= 700, String(out.idAfter));
  const minId = Math.min(...out.rows.map((r) => r.id));
  assert.ok(minId >= 700, "an id below base was issued before the first based walk: " + minId);
  assert.deepEqual(out.resolve699, { why: "stale" });
  assert.equal(out.idLater, out.idAfter, "a later base is ignored");
  t.diagnostic(`ACCEPT A8 ${keys.length + out.walks.length} calls answered exactly {why:"no base"}; media frame(1,0.1) -> ${JSON.stringify(out.mFrame)}, ` +
    `frames(1,[0.1,0.2]) -> ${JSON.stringify(out.mFrames)}; ` +
    `act(1) ${JSON.stringify(out.act)}; settle ${JSON.stringify(out.settle)}; first based walk ${JSON.stringify(out.based)}, min id ${minId}`);

  // pushState keeps the document and its ids; a navigation makes a new, un-based document
  await o.page.evaluate(() => history.pushState({}, "", "/register.html#step2"));
  const regId = () => o.ev(() => globalThis.__sb.id(Array.from(document.querySelectorAll("button")).find((b) => b.textContent === "Register")));
  assert.equal(await regId(), out.idAfter, "pushState keeps the ids");
  await o.page.goto(A.origin + "/popup.html", { waitUntil: "load" });
  const nav = await o.ev((i) => ({
    index: globalThis.__sb.index(), text: globalThis.__sb.text(), frames: globalThis.__sb.frames(),
    resolve: globalThis.__sb.resolve(i), walk: globalThis.__sb.walk({ dirty: true }),
  }), out.idAfter);
  assert.deepEqual(nav, { index: NB, text: NB, frames: NB, resolve: { why: "stale" }, walk: NB });
  const nb = await o.ev(() => ({ w: globalThis.__sb.walk({ base: 2000 }), rows: globalThis.__sb.index() }));
  assert.ok(nb.rows.length > 0 && nb.rows.every((r) => r.id >= 2000));

  // a same-origin child frame needs its own based walk
  const f = await open("frames.html");
  const sf = (await f.frames()).find((x) => x.url === A.origin + "/frame-child.html");
  assert.ok(sf);
  const child = await f.ev(() => ({ index: globalThis.__sb.index(), frames: globalThis.__sb.frames(), text: globalThis.__sb.text() }), undefined, sf.id);
  assert.deepEqual(child, { index: NB, frames: NB, text: NB });
  assert.ok(Array.isArray(await f.ev(() => globalThis.__sb.index())), "the parent is based");
  t.diagnostic(`ACCEPT A8 after goto: ${JSON.stringify(nav)}; child frame before its walk: ${JSON.stringify(child)}`);
});

// ------------------------------------------------------------------ A9 challenge page shapes
const TEXT_KEYS = new Set(["order", "frame", "kind", "level", "landmark", "heading", "spans"]);
const ROW_KEYS = new Set(["id", "frame", "role", "name", "state", "flags", "box"]);
async function challengePage(t, o, label) {
  const pre = await o.ev(() => ({ index: globalThis.__sb.index(), text: globalThis.__sb.text(), frames: globalThis.__sb.frames() }));
  assert.deepEqual(pre, { index: NB, text: NB, frames: NB }, label + ": no base comes before the challenge answer");
  const r = await o.ev(async () => {
    const sb = globalThis.__sb;
    const out = {};
    out.walk = sb.walk({ base: 1 });
    out.htmlId = sb.id(document.documentElement);
    out.index = sb.index();
    out.text = sb.text();
    out.textMain = sb.text("main");
    out.frames = sb.frames();
    const q = document.getElementById("q");
    const go = document.getElementById("go");
    out.nameQ = sb.name(q);
    out.stateQ = sb.state(q);
    out.nameGo = sb.name(go);
    out.goId = sb.id(go);
    out.actGo = await sb.act(out.goId);
    out.actPage = await sb.act(out.htmlId);
    out.fp = sb.fingerprint("button \"" + go.textContent + "\"");
    const b = document.createElement("button");
    b.textContent = "Late zqx";
    document.querySelector("main").append(b);
    out.walkDirty = sb.walk({ dirty: true });
    out.walkFull = sb.walk({});
    out.index2 = sb.index();
    out.vp = { w: innerWidth, h: innerHeight, cw: document.documentElement.clientWidth, ch: document.documentElement.clientHeight };
    out.host = location.hostname;
    return out;
  });
  const blob = JSON.stringify(r);
  assert.deepEqual(r.walk, { count: 1 }, label);
  assert.deepEqual(r.walkDirty, { count: 1 }, label);
  assert.deepEqual(r.walkFull, { count: 1 }, label);
  assert.equal(r.index.length, 1, label + " " + JSON.stringify(r.index));
  const row = r.index[0];
  assert.equal(row.id, r.htmlId, label + ": the row is the documentElement");
  assert.equal(row.role, "document");
  assert.equal(row.name, `challenge page (${r.host})`);
  assert.deepEqual(row.flags, { challenge: true });
  assert.ok(row.state === undefined || JSON.stringify(row.state) === "{}", JSON.stringify(row.state));
  assert.deepEqual(Object.keys(row).filter((k) => !ROW_KEYS.has(k)), [], label);
  assert.equal(row.frame, 0);
  assert.ok(row.box && row.box.x === 0 && row.box.y === 0, JSON.stringify(row.box));
  assert.ok([r.vp.w, r.vp.cw].includes(row.box.w) && [r.vp.h, r.vp.ch].includes(row.box.h), JSON.stringify({ box: row.box, vp: r.vp }));
  assert.deepEqual(r.index2, r.index, label + ": still one row after a DOM change");
  const res = spawnSync(process.execPath, [path.join(__dirname, "check-wire-shape.js"), dump("challenge-" + label, r.index)], { encoding: "utf8" });
  assert.equal(res.status, 0, res.stdout + res.stderr);
  // text(): one other block that links the row
  assert.equal(r.text.length, 1, label + " " + JSON.stringify(r.text));
  const tb = r.text[0];
  assert.equal(tb.kind, "other");
  assert.deepEqual(Object.keys(tb).filter((k) => !TEXT_KEYS.has(k)), []);
  assert.ok(tb.spans.length >= 1 && tb.spans.some((s) => s.elem === "#" + row.id), JSON.stringify(r.text));
  assert.ok(tb.spans.every((s) => s.elem === undefined || s.elem === "#" + row.id));
  assert.ok(Array.isArray(r.textMain), JSON.stringify(r.textMain));
  // frames(): only self, challenge true
  assert.equal(r.frames.length, 1, label + " " + JSON.stringify(r.frames));
  assert.equal(r.frames[0].frame, 0);
  assert.equal(r.frames[0].parent, null);
  assert.equal(r.frames[0].challenge, true);
  // nothing inside is read; acts refused
  assert.equal(r.nameQ, "");
  assert.equal(r.stateQ, null);
  assert.equal(r.nameGo, "");
  assert.deepEqual(r.actGo, { why: "challenge" });
  assert.deepEqual(r.actPage, { why: "challenge" });
  assert.equal(r.fp, "miss");
  assert.equal(/zqx|Inner|Continue/.test(blob), false, label + ": page content leaked: " + blob);
  t.diagnostic(`ACCEPT A9 ${label}: index ${JSON.stringify(r.index)} text ${JSON.stringify(r.text)} textMain ${JSON.stringify(r.textMain)} ` +
    `frames ${JSON.stringify(r.frames)} act ${JSON.stringify(r.actGo)}`);
}

test("A9 challenge page (D-73, D-424): one CHALLENGE document row, one other text block, frames() self only; nothing inside read", async (t) => {
  // control: the same page on an ordinary path is read normally
  const n = await open("accept-challenge-page.html");
  const nr = fill(await n.ev(() => globalThis.__sb.index()));
  assert.ok(byName(nr, "Inner go", "button") && !nr.some((r) => r.flags.challenge), JSON.stringify(nr.map(sig)));
  assert.match(flat(await n.ev(() => globalThis.__sb.text())), /zqx/);
  const nf = await n.ev(() => globalThis.__sb.frames());
  assert.equal(nf.length, 2);
  assert.equal(nf[0].challenge, false);
  // a challenge path and the interstitial title
  await challengePage(t, await open("cdn-cgi/challenge-platform/accept-challenge-page.html", { base: null }), "path");
  await challengePage(t, await open("accept-just-a-moment.html", { base: null }), "title");
});

// ------------------------------------------------------------------ A10 password canary
test("A10 password value never in index()/text()/state()/name() after type swap, -webkit-text-security, autocomplete, pw- name, rebuilds (D-405 (1), D-426, D-429 (1))", async (t) => {
  const secret = process.env.SB_VAULT_CANARY || "sbpw" + crypto.randomBytes(12).toString("hex");
  const o = await open("accept-pw.html", { base: null });
  const SENS = ["p1", "p2", "m1", "m2", "ac", "nm", "r1", "w-in", "r3", "plain-copy", "wrap-copy", "appended-copy"];
  // trap the value getter in world sb before anything reads
  await o.ev((ids) => {
    const d = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value");
    globalThis.__pwReads = {};
    Object.defineProperty(HTMLInputElement.prototype, "value", {
      configurable: true,
      enumerable: d.enumerable,
      get() {
        if (ids.includes(this.id)) globalThis.__pwReads[this.id] = (globalThis.__pwReads[this.id] || 0) + 1;
        return d.get.call(this);
      },
      set(v) {
        d.set.call(this, v);
      },
    });
  }, SENS);
  await o.page.fill("#nick", "ada");
  await o.page.fill("#p1", secret.slice(0, 6));
  await o.page.locator("#p1").pressSequentially(secret.slice(6));
  for (const id of ["p2", "m1", "m2", "ac", "nm", "r1", "w-in", "r3"]) await o.page.fill("#" + id, secret);
  // p2's type swaps before the first walk (D-426: the observer saw the old type)
  await o.page.evaluate(() => { document.getElementById("p2").type = "text"; });
  const read = (lbl) => o.ev(async (l) => {
    const sb = globalThis.__sb;
    const r = { label: l };
    r.walkDirty = sb.walk({ dirty: true });
    r.index = sb.index();
    r.text = sb.text();
    r.textF1 = sb.text("f1");
    r.frames = sb.frames();
    r.fp = sb.fingerprint("textbox \"Password one\" textbox \"Shown field\"");
    r.per = {};
    for (const el of document.querySelectorAll("input")) {
      const id = sb.id(el);
      r.per[el.id] = { id, name: sb.name(el), role: sb.role(el), state: sb.state(el), act: await sb.act(id) };
    }
    r.settle = await sb.settle({ cap: 400 });
    r.walkFull = sb.walk({});
    r.index2 = sb.index();
    r.text2 = sb.text();
    return r;
  }, lbl);
  await o.ev(() => globalThis.__sb.walk({ base: 1 }));
  const r1 = await read("before");
  // the nick control below needs the swaps to land after D-442's 2 s window (see A18)
  await pastBornWindow(o);
  // the site's own handlers: show toggle and three rebuilds; masking and autocomplete dropped
  await o.page.click("#btn-show");
  await o.page.click("#btn-rebuild");
  await o.page.click("#btn-wrap");
  await o.page.click("#btn-append");
  await o.page.evaluate(() => {
    document.getElementById("m1").classList.remove("masked");
    document.getElementById("m2").removeAttribute("style");
    document.getElementById("ac").removeAttribute("autocomplete");
  });
  const facts = await o.page.evaluate(() => ({
    p1: document.getElementById("p1").type,
    copies: ["plain-copy", "wrap-copy", "appended-copy"].map((i) => document.getElementById(i).value.length),
    gone: ["r1", "w-in", "r3"].filter((i) => document.getElementById(i)).length,
  }));
  assert.equal(facts.p1, "text");
  assert.deepEqual(facts.copies, [secret.length, secret.length, secret.length], "the rebuilt fields carry the value");
  assert.equal(facts.gone, 0);
  // more real typing into the toggled field (input events after the swap)
  await o.page.locator("#p1").pressSequentially("zz");
  const r2 = await read("after");
  const reads = await o.ev(() => globalThis.__pwReads);
  const blob = JSON.stringify([r1, r2]);
  const df = dump("pw-canary", [r1, r2]);
  assert.deepEqual(reads, {}, "world sb called the value getter of a sensitive field: " + JSON.stringify(reads));
  assert.equal(blob.includes(secret), false, "secret found in __sb output");
  if (secret.length >= 8) {
    for (let i = 0; i + 8 <= secret.length; i += 2) {
      assert.equal(blob.includes(secret.slice(i, i + 8)), false, "a fragment of the secret found in __sb output");
    }
  }
  const kinds = {};
  for (const r of [r1, r2]) {
    for (const [id, p] of Object.entries(r.per)) {
      if (id === "nick") continue;
      assert.ok(p.state && p.state.value && ["empty", "filled"].includes(p.state.value.kind) && !("text" in p.state.value),
        `${r.label} ${id} ${JSON.stringify(p.state)}`);
      kinds[r.label + ":" + id] = p.state.value.kind;
      const row = r.index2.find((x) => x.id === p.id);
      assert.ok(row, `${r.label} ${id} has a row`);
      assert.ok(row.state && row.state.value && ["empty", "filled"].includes(row.state.value.kind), `${r.label} ${id} row ${JSON.stringify(row.state)}`);
      assert.equal(typeof p.act.x, "number", `${r.label} ${id} act ${JSON.stringify(p.act)}`);
    }
    assert.deepEqual(r.per.nick.state.value, { kind: "text", text: "ada" }, "control: the ordinary field is read");
  }
  for (const id of ["p1", "p2", "m1", "m2", "ac", "nm", "r1", "w-in", "r3"]) assert.equal(kinds["before:" + id], "filled", "before " + id);
  for (const id of ["p1", "p2", "m1", "m2", "ac", "nm", "plain-copy", "wrap-copy", "appended-copy"]) {
    assert.equal(kinds["after:" + id], "filled", "after " + id);
  }
  t.diagnostic(`ACCEPT A10 secret len ${secret.length}${process.env.SB_VAULT_CANARY ? " (SB_VAULT_CANARY)" : ""} absent from ${blob.length} bytes ` +
    `(dump ${path.basename(df)}); sb value-getter reads on sensitive fields ${JSON.stringify(reads)}; kinds ${JSON.stringify(kinds)}`);
});

// ------------------------------------------------------------------ A11 D-429 challenge text
test("A11 challenge text (D-429 (2), D-422): press-and-hold UI and an Are-you-human quiz are not CHALLENGE; widgets and real prompts are", async (t) => {
  const o = await open("accept-challenge-text.html");
  const idOf = (sel) => o.ev((s) => globalThis.__sb.id(document.querySelector(s)), sel);
  const rows0 = fill(await o.ev(() => globalThis.__sb.index()));
  assert.deepEqual(rows0.filter((r) => r.flags.challenge).map(sig), [], "no CHALLENGE row on ordinary UI");
  assert.ok(byName(rows0, "Are you human?", "heading"));
  const f0 = await o.ev(() => globalThis.__sb.frames());
  assert.equal(f0[0].challenge, false);
  const text0 = flat(await o.ev(() => globalThis.__sb.text()));
  for (const re of [/Press and hold Shift/, /mic button to talk/, /record a voice message/, /what makes us human/]) assert.match(text0, re);
  for (const sel of ["#files-in", "#mic", "#hold", "#start"]) {
    const a = await timedAct(o, await idOf(sel));
    assert.equal(typeof a.r.x, "number", sel + " " + JSON.stringify(a.r));
  }
  // positives added to the same page: an hCaptcha box, PerimeterX press-and-hold, strong prompts
  await o.page.evaluate(() => {
    document.getElementById("extra").innerHTML =
      "<div id=\"hc\" class=\"h-captcha\" data-sitekey=\"fixture\" style=\"width:300px;height:60px\"></div>" +
      "<div id=\"px\"><p id=\"px-p\">Press &amp; Hold to confirm you are a human (and not a bot).</p>" +
      "<div id=\"px-captcha\" style=\"width:300px;height:50px\"></div></div>" +
      "<p id=\"robot\">Are you a robot?</p><p id=\"vh\">Verify you are human</p>";
  });
  await o.ev(() => globalThis.__sb.walk({ dirty: true }));
  const rows1 = fill(await o.ev(() => globalThis.__sb.index()));
  const ch1 = new Set(rows1.filter((r) => r.flags.challenge).map((r) => r.id));
  const chNames = JSON.stringify(rows1.filter((r) => r.flags.challenge).map(sig));
  for (const sel of ["#hc", "#px-p", "#px-captcha", "#robot", "#vh"]) assert.ok(ch1.has(await idOf(sel)), sel + " is CHALLENGE: " + chNames);
  assert.ok(byName(rows1, "Are you human?", "heading").flags.challenge, "with a widget on the page the quiz heading is a prompt (D-429 (2))");
  // D-435 (3): with a widget on the page press-and-hold text is a prompt too; the controls stay ordinary
  for (const n of ["Press and hold Shift to select several files.", "Press and hold the mic button to talk"]) {
    assert.ok(byName(rows1, n) && byName(rows1, n).flags.challenge, n + ": " + chNames);
  }
  assert.ok(!rows1.some((r) => r.flags.challenge && /^(Hold to talk|Mic|Files|Start the quiz)$/.test(r.name)), chNames);
  assert.deepEqual((await timedAct(o, await idOf("#px-captcha"))).r, { why: "challenge" });
  assert.equal(typeof (await timedAct(o, await idOf("#mic"))).r.x, "number", "the mic button stays actionable next to a real widget");
  // widgets removed again: back to no CHALLENGE (the context change forces a full walk)
  await o.page.evaluate(() => { document.getElementById("extra").textContent = ""; });
  await o.ev(() => globalThis.__sb.walk({ dirty: true }));
  const rows2 = fill(await o.ev(() => globalThis.__sb.index()));
  assert.deepEqual(rows2.filter((r) => r.flags.challenge).map(sig), []);

  // bench/pages challenge fixtures
  const it = await open("challenge-interstitial.html");
  const ir = fill(await it.ev(() => globalThis.__sb.index()));
  const ich = ir.filter((r) => r.flags.challenge);
  assert.equal(ich.length, 2, JSON.stringify(ich.map(sig)));
  assert.ok(ich.some((r) => r.name === "Verify you are human by completing the action below."));
  const wHost = await it.ev(() => globalThis.__sb.id(document.getElementById("widget")));
  assert.ok(ich.some((r) => r.id === wHost), "the closed-shadow widget host is CHALLENGE");
  for (const r of ich) assert.deepEqual((await timedAct(it, r.id)).r, { why: "challenge" });
  const help = byName(ir, "Why am I seeing this page?", "link");
  assert.ok(help && !help.flags.challenge);
  assert.equal(typeof (await timedAct(it, help.id)).r.x, "number");

  const w = await open("challenge-widgets.html");
  const wid = (sel) => w.ev((s) => globalThis.__sb.id(document.querySelector(s)), sel);
  const wr = fill(await w.ev(() => globalThis.__sb.index()));
  const wch = wr.filter((r) => r.flags.challenge).map((r) => r.id).sort((a, b) => a - b);
  assert.deepEqual(wch, [await wid("#arkose iframe"), await wid(".geetest_holder"), await wid("#hc")].sort((a, b) => a - b));
  for (const id of wch) assert.deepEqual((await timedAct(w, id)).r, { why: "challenge" });
  for (const n of ["Username", "Sign in"]) {
    const r = byName(wr, n);
    assert.ok(r && !r.flags.challenge, n);
    assert.equal(typeof (await timedAct(w, r.id)).r.x, "number", n);
  }
  const wf = await w.ev(() => globalThis.__sb.frames());
  const ark = wf.find((f) => f.name === "Verification challenge");
  const badge = wf.find((f) => f.name === "reCAPTCHA");
  assert.ok(ark && ark.challenge === true, JSON.stringify(wf));
  assert.ok(badge && badge.challenge === false, JSON.stringify(wf));
  assert.equal(wf[0].challenge, false);

  const art = await open("how-captchas-work.html");
  const ar = fill(await art.ev(() => globalThis.__sb.index()));
  assert.ok(!ar.some((r) => r.flags.challenge), JSON.stringify(ar.filter((r) => r.flags.challenge).map(sig)));
  assert.match(flat(await art.ev(() => globalThis.__sb.text())), /not a robot/);
  t.diagnostic(`ACCEPT A11 negatives: 0 CHALLENGE rows of ${rows0.length}; positives: ${ch1.size} CHALLENGE rows after widgets (${chNames}); ` +
    `interstitial ${ich.length} CHALLENGE (acts refused), widgets ${wch.length} CHALLENGE, badge frame challenge=${badge.challenge}, article 0`);
});

// ------------------------------------------------------------------ round 3 (tester-8)
// A12 D-435 (3) on accept-challenge-text.html; A13 D-436 on accept-v3-quiz.html; A14 D-435 (1)(2)
// and D-437 on accept-d435.html with the password canary; A15 D-82 on accept-media-refusal.html.

const chRows = (rows) => rows.filter((r) => r.flags.challenge);
const indexOf = async (o) => fill(await o.ev(() => globalThis.__sb.index()));
const actOk = async (o, row, why) => {
  assert.ok(row, why + ": no row");
  assert.ok(!row.flags.challenge, why + " is CHALLENGE");
  const a = await timedAct(o, row.id);
  assert.equal(typeof a.r.x, "number", why + " act " + JSON.stringify(a.r));
};

test("A12 D-435 (3): with only a challenge widget on the page, press-and-hold lines are CHALLENGE; Files, Mic, Hold to talk and Start stay ordinary rows", async (t) => {
  const o = await open("accept-challenge-text.html");
  const HOLD = ["Press and hold Shift to select several files.", "Press and hold the mic button to talk", "Press & hold to record a voice message."];
  const CONTROLS = [["Files"], ["Mic", "button"], ["Hold to talk", "button"], ["Start the quiz", "button"]];
  const rows0 = await indexOf(o);
  // without a widget a hold line is plain text: no CHALLENGE row, and text() reads it
  assert.deepEqual(chRows(rows0).map(sig), []);
  const text0 = flat(await o.ev(() => globalThis.__sb.text()));
  for (const n of HOLD) assert.ok(text0.includes(n), n + " in text() without a widget");
  // only a widget, no human or robot words anywhere new: the challenge context alone decides
  await o.page.evaluate(() => {
    document.getElementById("extra").innerHTML = "<div id=\"hc\" class=\"h-captcha\" data-sitekey=\"fixture\" style=\"width:300px;height:60px\"></div>";
  });
  await o.ev(() => globalThis.__sb.walk({ dirty: true }));
  const rows1 = await indexOf(o);
  const names1 = JSON.stringify(chRows(rows1).map(sig));
  for (const n of HOLD) assert.ok(byName(rows1, n) && byName(rows1, n).flags.challenge, n + " with a widget: " + names1);
  assert.ok(byName(rows1, "Are you human?", "heading").flags.challenge, names1);
  for (const [n, role] of CONTROLS) await actOk(o, byName(rows1, n, role), n);
  const hc = await o.ev(() => globalThis.__sb.id(document.getElementById("hc")));
  assert.ok(chRows(rows1).some((r) => r.id === hc), "the widget box is CHALLENGE");
  assert.equal(chRows(rows1).length, HOLD.length + 2, "hold lines + heading + widget, nothing else: " + names1);
  await o.page.evaluate(() => { document.getElementById("extra").textContent = ""; });
  await o.ev(() => globalThis.__sb.walk({ dirty: true }));
  const rows2 = await indexOf(o);
  assert.deepEqual(chRows(rows2).map(sig), []);
  t.diagnostic(`ACCEPT A12 widget only: ${chRows(rows1).length} CHALLENGE rows ${names1}; controls ${CONTROLS.map((c) => c[0]).join(", ")} ordinary and actionable; removed: 0`);
});

test("A13 D-436: an Are-you-human quiz is not CHALLENGE next to passive v3 markers; it is next to a visible v2 widget, frame or response field", async (t) => {
  const report = {};
  const check = async (v) => {
    const o = await open("accept-v3-quiz.html?v=" + v);
    assert.equal(await o.page.evaluate(() => document.documentElement.getAttribute("data-variant")), v);
    const rows = await indexOf(o);
    const frames = await o.ev(() => globalThis.__sb.frames());
    const text = flat(await o.ev(() => globalThis.__sb.text()));
    report[v] = chRows(rows).map(sig);
    return { o, rows, frames, text };
  };
  for (const v of ["none", "badge", "bound", "script", "scriptshown", "invisible", "hidden", "v3all"]) {
    const { o, rows, frames, text } = await check(v);
    assert.deepEqual(chRows(rows).map(sig), [], v + ": no CHALLENGE row");
    assert.ok(byName(rows, "Are you human?", "heading"), v + " heading row");
    assert.equal(frames[0].challenge, false, v);
    assert.match(text, /Are you human\?/, v);
    assert.match(text, /Press and hold the button to start/, v);
    await actOk(o, byName(rows, "Start the quiz", "button"), v + " Start");
    await actOk(o, byName(rows, "Send", "button"), v + " Send");
    await o.close();
  }
  for (const v of ["v2", "v2frame", "resp", "v3v2"]) {
    const { o, rows } = await check(v);
    const names = JSON.stringify(report[v]);
    assert.ok(byName(rows, "Are you human?", "heading").flags.challenge, v + ": heading is CHALLENGE " + names);
    const hold = byName(rows, "Press and hold the button to start");
    assert.ok(hold && hold.flags.challenge, v + ": hold line is CHALLENGE " + names);
    await actOk(o, byName(rows, "Start the quiz", "button"), v + " Start");
    await actOk(o, byName(rows, "Send", "button"), v + " Send");
    await actOk(o, rows.find((r) => r.name === "Email" && r.role === "textbox"), v + " Email");
    if (v === "v2" || v === "v3v2") {
      const box = chRows(rows).filter((r) => r.name !== "Are you human?" && r.name !== "Press and hold the button to start");
      assert.ok(box.length >= 1, v + " widget row " + names);
      for (const r of box) assert.deepEqual((await timedAct(o, r.id)).r, { why: "challenge" }, v + " " + sig(r));
    }
    await o.close();
  }
  // live: the badge page gains a v2 checkbox, then loses it again
  const { o } = await check("badge");
  await o.page.evaluate(() => {
    document.getElementById("extra").innerHTML = "<div id=\"v2\" class=\"g-recaptcha\" data-sitekey=\"fixture\" style=\"width:304px;height:78px\">" +
      "<iframe title=\"reCAPTCHA\" width=\"304\" height=\"78\" srcdoc=\"&lt;p&gt;x&lt;/p&gt;\"></iframe></div>";
  });
  await o.ev(() => globalThis.__sb.walk({ dirty: true }));
  assert.ok(byName(await indexOf(o), "Are you human?", "heading").flags.challenge, "badge page + v2 checkbox");
  await o.page.evaluate(() => { document.getElementById("extra").textContent = ""; });
  await o.ev(() => globalThis.__sb.walk({ dirty: true }));
  assert.deepEqual(chRows(await indexOf(o)).map(sig), [], "v2 removed, only the badge left");
  t.diagnostic("ACCEPT A13 CHALLENGE rows per variant " + JSON.stringify(report));
});

test("A14 D-435 (1)(2), D-437: SPA swap keeps unrelated pre-filled fields readable; show-password toggles never emit the value", async (t) => {
  const secret = process.env.SB_VAULT_CANARY || "sbpw" + crypto.randomBytes(12).toString("hex");
  const o = await open("accept-d435.html");
  const SENS = ["t-type", "t-same", "t-neu", "t-wrap", "t-app", "t-mic", "t-to1", "t-to2", "a-pw", "l-word"];
  const NEW = ["t-same", "shown-b", "shown-c", "shown-d", "shown-e", "shown-f", "shown-g", "l-word"];
  await o.ev((ids) => {
    const d = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value");
    globalThis.__pwReads = {};
    Object.defineProperty(HTMLInputElement.prototype, "value", {
      configurable: true,
      enumerable: d.enumerable,
      get() {
        if (ids.includes(this.id)) globalThis.__pwReads[this.id] = (globalThis.__pwReads[this.id] || 0) + 1;
        return d.get.call(this);
      },
      set(v) {
        d.set.call(this, v);
      },
    });
  }, SENS.concat(NEW));
  await o.page.fill("#nick", "ada");
  await o.page.fill("#a-email", "ada@example.org");
  await o.page.fill("#l-email", "ada@example.org");
  await o.page.locator("#t-type").pressSequentially(secret);
  for (const id of SENS.slice(1)) await o.page.fill("#" + id, secret);
  const read = (lbl) => o.ev(async (l) => {
    const sb = globalThis.__sb;
    const r = { label: l };
    r.walkDirty = sb.walk({ dirty: true });
    r.index = sb.index();
    r.text = sb.text();
    r.frames = sb.frames();
    r.fp = sb.fingerprint("textbox \"Type toggle\" textbox \"Memorable word\"");
    r.per = {};
    for (const el of document.querySelectorAll("input,textarea")) {
      const id = sb.id(el);
      r.per[el.id] = { id, name: sb.name(el), role: sb.role(el), state: sb.state(el), act: await sb.act(id) };
    }
    r.settle = await sb.settle({ cap: 400 });
    r.walkFull = sb.walk({});
    r.index2 = sb.index();
    r.text2 = sb.text();
    return r;
  }, lbl);
  const r1 = await read("before");
  // the nick control below needs the swaps to land after D-442's 2 s window (see A18)
  await pastBornWindow(o);
  // each toggle in its own task, so each is its own observer batch (the microtask one splits itself)
  for (const fn of ["toggleType", "toggleSame", "toggleNeutral", "toggleWrap", "toggleAppend", "toggleMicro", "toggleTo1", "toggleTo2"]) {
    await o.page.evaluate((f) => globalThis[f](), fn);
  }
  await o.page.waitForFunction(() => !!document.getElementById("shown-f") && !!document.getElementById("shown-g"), null, { timeout: 5000 });
  await o.page.click("#sign-in");
  await o.page.click("#rerender");
  await o.page.locator("#t-type").pressSequentially("zz");
  const facts = await o.page.evaluate(() => ({
    gone: ["t-neu", "t-wrap", "t-app", "t-mic", "t-to1", "t-to2", "a-pw"].filter((i) => document.getElementById(i)).length,
    lens: ["t-same", "shown-b", "shown-c", "shown-d", "shown-e", "shown-f", "shown-g", "l-word"].map((i) => document.getElementById(i).value.length),
    types: ["t-type", "t-same", "l-word"].map((i) => document.getElementById(i).type),
  }));
  assert.equal(facts.gone, 0);
  assert.deepEqual(facts.lens, Array(8).fill(secret.length), "the toggled fields carry the value");
  assert.deepEqual(facts.types, ["text", "text", "text"]);
  const r2 = await read("after");
  // D-443 (4): the profile view and the re-rendered login form went into <main>, which still
  // holds sensitive fields, so they read empty|filled for 2 s from their add; the D-435 (1)
  // reads below come from r3, after that window (a field wrongly marked would stay filled)
  await pastBornWindow(o, await o.page.evaluate(() => performance.now()));
  const r3 = await read("after window");
  const reads = await o.ev(() => globalThis.__pwReads);
  const blob = JSON.stringify([r1, r2, r3]);
  const df = dump("d435-canary", [r1, r2, r3]);
  assert.deepEqual(reads, {}, "world sb called the value getter of a sensitive field: " + JSON.stringify(reads));
  assert.equal(blob.includes(secret), false, "secret found in __sb output");
  for (let i = 0; i + 8 <= secret.length; i += 2) {
    assert.equal(blob.includes(secret.slice(i, i + 8)), false, "a fragment of the secret found in __sb output");
  }
  const kind = (r, id) => r.per[id] && r.per[id].state && r.per[id].state.value;
  // failure messages carry kinds only, never a state that could hold the value
  const kindOf = (v) => (v ? v.kind : String(v));
  for (const id of SENS) assert.equal(kindOf(kind(r1, id)), "filled", "before " + id);
  for (const id of ["t-type"].concat(NEW)) {
    assert.equal(kindOf(kind(r2, id)), "filled", "after " + id);
    assert.equal(Object.keys(kind(r2, id)).join(","), "kind", "after " + id + " carries only a kind");
    const row = r2.index2.find((x) => x.id === r2.per[id].id);
    assert.ok(row && row.state && row.state.value && row.state.value.kind === "filled", id + " row kind " + (row && row.state && kindOf(row.state.value)));
  }
  // D-435 (1): the swapped-in profile view and fields outside the swap read their values
  const READ = { dn: "Ada Lovelace", city: "London", bio: "Mathematician and writer", phone: "+44 20 7946 0000",
    "l-email": "ada@example.org", q: "pre-filled query", nick: "ada" };
  // tester-9 (round 4): q (in the header) and nick (parsed into the form, not re-added, the swaps
  // come after its 2 s window) are not added next to a live password, so D-443 (4) does not
  // cover them: they read their text right after the swaps, not only after the window
  const UNTOUCHED = ["q", "nick"];
  for (const [id, text] of Object.entries(READ)) {
    const k2 = kind(r2, id);
    if (UNTOUCHED.includes(id)) assert.deepEqual(k2, { kind: "text", text }, "after (untouched) " + id);
    assert.ok(k2 && (k2.kind === "empty" || k2.kind === "filled" || (k2.kind === "text" && k2.text === text)), "after " + id + " " + kindOf(k2));
    assert.deepEqual(kind(r3, id), { kind: "text", text }, "after window " + id);
  }
  t.diagnostic(`ACCEPT A14 secret len ${secret.length}${process.env.SB_VAULT_CANARY ? " (SB_VAULT_CANARY)" : ""} absent from ${blob.length} bytes ` +
    `(dump ${path.basename(df)}); value-getter reads ${JSON.stringify(reads)}; filled after: t-type ${NEW.join(" ")}; read after: ${Object.keys(READ).join(" ")}`);
});

test("A15 D-82: __sbMedia whole-call refusals are frame() -> {t:null, why} and frames() -> one {t:null, why} entry; per-time failures keep one entry per time", async (t) => {
  const o = await open("accept-media-refusal.html", { base: null, media: true });
  const pre = await o.ev(async () => {
    const m = globalThis.__sbMedia;
    return {
      frame: JSON.stringify(await m.frame(1, 0.1)),
      frames2: JSON.stringify(await m.frames(1, [0.1, 0.2])),
      frames4: JSON.stringify(await m.frames(5, [0.1, 0.2, 0.3, 0.4])),
      frames0: JSON.stringify(await m.frames(1, [])),
    };
  });
  assert.equal(pre.frame, "{\"t\":null,\"why\":\"no base\"}");
  assert.equal(pre.frames2, "[{\"t\":null,\"why\":\"no base\"}]");
  assert.equal(pre.frames4, "[{\"t\":null,\"why\":\"no base\"}]");
  assert.equal(pre.frames0, "[{\"t\":null,\"why\":\"no base\"}]");
  await o.ev(() => globalThis.__sb.walk({ base: 300 }));
  const ids = await o.ev(() => ({
    bad: globalThis.__sb.id(document.getElementById("bad")),
    cap: globalThis.__sb.id(document.getElementById("cap")),
    para: globalThis.__sb.id(document.getElementById("para")),
  }));
  const post = await o.ev(async (a) => {
    const m = globalThis.__sbMedia;
    const s = (x) => JSON.stringify(x);
    return {
      capFrame: s(await m.frame(a.cap, 0.5)),
      capFrames: s(await m.frames(a.cap, [0.1, 0.2, 0.3])),
      paraFrame: s(await m.frame(a.para, 0.1)),
      paraFrames: s(await m.frames(a.para, [0.1, 0.2])),
      goneFrames: s(await m.frames(99999, [0.1, 0.2])),
      goneFrame: s(await m.frame(99999, 0.1)),
      staleFrames: s(await m.frames(5, [0.1, 0.2])),
      list: m.list().map((v) => v.id),
      bad: await m.frames(a.bad, [0.1, 0.2, 0.3], { seekCapMs: 1500, budgetMs: 6000, hashOnly: true }),
      badOne: await m.frame(a.bad, 0.25, { seekCapMs: 1500, budgetMs: 3000 }),
    };
  }, ids);
  assert.equal(post.capFrame, "{\"t\":null,\"why\":\"challenge\"}");
  assert.equal(post.capFrames, "[{\"t\":null,\"why\":\"challenge\"}]");
  assert.equal(post.paraFrame, "{\"t\":null,\"why\":\"not a video\"}");
  assert.equal(post.paraFrames, "[{\"t\":null,\"why\":\"not a video\"}]");
  for (const k of ["goneFrames", "staleFrames"]) {
    const v = JSON.parse(post[k]);
    assert.equal(v.length, 1, k + " " + post[k]);
    assert.deepEqual(Object.keys(v[0]), ["t", "why"], k);
    assert.equal(v[0].t, null, k);
    assert.ok(["gone", "stale"].includes(v[0].why), k + " " + post[k]);
  }
  assert.deepEqual(JSON.parse(post.goneFrame), JSON.parse(post.goneFrames)[0]);
  assert.ok(post.list.includes(ids.bad) && !post.list.includes(ids.cap) && !post.list.includes(ids.para), JSON.stringify(post.list));
  // per-time failures: one entry per requested time, each with its own t (never t:null)
  assert.equal(post.bad.length, 3, JSON.stringify(post.bad));
  assert.deepEqual(post.bad.map((e) => e.t), [0.1, 0.2, 0.3], JSON.stringify(post.bad));
  for (const e of post.bad) assert.ok(typeof e.why === "string" && e.why && !("hash" in e), JSON.stringify(e));
  assert.equal(post.badOne.t, 0.25, JSON.stringify(post.badOne));
  assert.equal(typeof post.badOne.why, "string");
  t.diagnostic(`ACCEPT A15 before base ${pre.frame} ${pre.frames2} (2 times) ${pre.frames4} (4 times); challenge ${post.capFrame} ${post.capFrames} (3 times); ` +
    `not a video ${post.paraFrames}; unknown ${post.goneFrames}; below base ${post.staleFrames}; per-time ${JSON.stringify(post.bad)}; frame ${JSON.stringify(post.badOne)}`);
});

// A16: D-438 (1)(2) (review-T-19 round 4 Blocking and Should) and the D-437 "lost exactly one
// field" rule. Values are checked in world sb; only kinds, counts and booleans come back, so a
// leak fails the test without the value reaching the test output.
test("A16 D-438: add-first rebuilds (later task, microtask, transitionend, overlap) and a wrapper with a show checkbox never emit the value; a two-field to one-field page still reads", async (t) => {
  const secret = process.env.SB_VAULT_CANARY || "sbpw" + crypto.randomBytes(12).toString("hex");
  const o = await open("accept-d438.html");
  const OLD = ["af-task", "af-mic", "af-fade", "af-ov", "af-neu", "af-far", "wc-pw", "two-pw"];
  const NEW = ["af-task-new", "af-mic-new", "af-fade-new", "af-ov-new", "af-neu-new", "af-far-new", "wc-new"];
  await o.ev((ids) => {
    const d = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value");
    globalThis.__pwReads = {};
    Object.defineProperty(HTMLInputElement.prototype, "value", {
      configurable: true,
      enumerable: d.enumerable,
      get() {
        if (ids.includes(this.id)) globalThis.__pwReads[this.id] = (globalThis.__pwReads[this.id] || 0) + 1;
        return d.get.call(this);
      },
      set(v) {
        d.set.call(this, v);
      },
    });
  }, OLD.concat(NEW));
  await o.page.fill("#two-email", "ada@example.org");
  for (const id of OLD) await o.page.fill("#" + id, secret);
  // world sb read: every reader, then per-field kinds and whether the value appears anywhere
  const probe = (ids) => o.ev(async (a) => {
    const sb = globalThis.__sb;
    const out = [sb.walk({ dirty: true }), sb.index(), sb.text(), sb.frames()];
    const per = {};
    for (const id of a.ids) {
      const el = document.getElementById(id);
      if (!el) {
        per[id] = { missing: true };
        continue;
      }
      const s = sb.state(el);
      const x = [s, sb.name(el), sb.id(el), await sb.act(sb.id(el))];
      out.push(x);
      per[id] = { kind: s && s.value ? s.value.kind : String(s && s.value), value: JSON.stringify(x).includes(a.secret) };
    }
    out.push(sb.walk({}), sb.index(), sb.text());
    const blob = JSON.stringify(out);
    let frag = false;
    for (let i = 0; i + 8 <= a.secret.length; i += 2) if (blob.includes(a.secret.slice(i, i + 8))) frag = true;
    return { per, value: blob.includes(a.secret), frag, bytes: blob.length };
  }, { ids, secret });
  await o.ev(() => globalThis.__sb.index());
  // overlap: the copy is in, the old field still there (removed after 1500 ms)
  await o.page.evaluate(() => globalThis.addFirstOverlap());
  const ov = await probe(["af-ov-new"]);
  for (const fn of ["addFirstTask", "addFirstMicro", "addFirstFade", "addFirstNeutral", "addFirstFar", "wrapCheckbox", "twoToOne"]) {
    await o.page.evaluate((f) => globalThis[f](), fn);
  }
  const twoToOneAt = await o.page.evaluate(() => performance.now());
  await o.page.waitForFunction((ids) => ids.every((i) => !document.getElementById(i)), OLD, { timeout: 5000 });
  const after = await probe(NEW.concat(["promo"]));
  // D-443 (4): #one went into <main>, which still held the rebuilt copies: the promo code reads
  // empty|filled for 2 s from its add, so its D-437 control reads after that window
  await pastBornWindow(o, twoToOneAt);
  const reads = await o.ev(() => globalThis.__pwReads);
  const promo = await o.ev(() => globalThis.__sb.state(document.getElementById("promo")).value);
  const leaked = (p, id) => !!(p.per[id] && (p.per[id].value || p.per[id].kind !== "filled"));
  const leaks = Array.from(new Set(Object.keys(reads).concat(NEW.filter((id) => leaked(after, id)))));
  if (leaked(ov, "af-ov-new")) leaks.push("af-ov-new (overlap)");
  t.diagnostic(`ACCEPT A16 secret len ${secret.length}${process.env.SB_VAULT_CANARY ? " (SB_VAULT_CANARY)" : ""}; overlap kind ${ov.per["af-ov-new"].kind}; ` +
    `after kinds ${JSON.stringify(Object.fromEntries(NEW.map((i) => [i, after.per[i].kind])))}; value-getter reads ${JSON.stringify(reads)}; ` +
    `value in output: overlap ${ov.value || ov.frag}, after ${after.value || after.frag} (${after.bytes} bytes); promo kind ${promo && promo.kind}; leaking fields: ${leaks.join(", ") || "none"}`);
  // D-437 negative: two fields lost, one gained: not a swap, the promo code reads
  assert.deepEqual(promo, { kind: "text", text: "SPRING" }, "two-field to one-field page");
  assert.deepEqual(leaks, [], "fields whose value sb read or emitted: " + leaks.join(", "));
  assert.equal(ov.value || ov.frag || after.value || after.frag, false, "the value or a fragment is in __sb output");
});

// A18: D-435 (1) against D-438 (1)'s same-parent rule: a wrapper swap shortly after load must
// not mark an untouched ordinary field of the same form. A10 and A14 do much typing first, so
// whether their swap lands inside the 2 s window varies; this test swaps at once.
test("A18 D-442, B-415: a show-password wrapper swap within 2 s of load over-marks the parser-inserted ordinary field of the same form; after 2 s it reads", async (t) => {
  const out = {};
  for (const [page, fill, swap] of [["accept-pw.html", "#p1", "#btn-wrap"], ["accept-d435.html", "#t-wrap", null]]) {
    for (const late of [false, true]) {
      const o = await open(page);
      const t0 = await o.page.evaluate(() => performance.now());
      await o.page.fill("#nick", "ada");
      await o.page.fill(fill, "pw-dummy-a18");
      if (late) await pastBornWindow(o);
      if (swap) await o.page.click(swap);
      else await o.page.evaluate(() => globalThis.toggleWrap());
      // performance.now() after the swap bounds the swap time from above; nick was inserted
      // after time 0, so at <= 2000 the swap is inside its 2 s window
      const at = await o.page.evaluate(() => performance.now());
      const v = await o.ev(() => {
        globalThis.__sb.walk({ dirty: true });
        return globalThis.__sb.state(document.getElementById("nick")).value;
      });
      const key = page + (late ? " late" : "");
      out[key] = { ms: Math.round(at - t0), at: Math.round(at), kind: v && v.kind };
      if (late) {
        assert.deepEqual(v, { kind: "text", text: "ada" }, key + ": Nickname after the 2 s window");
      } else if (at <= 2000) {
        // the accepted over-mark: empty|filled, never the text (page.fill leaves caret evidence)
        assert.deepEqual(v, { kind: "filled" }, key + ": Nickname, swap " + Math.round(at) + " ms after navigation start");
      } else {
        // a slow machine: the swap may fall outside the window; either reading is allowed, and
        // the diagnostic shows which
        assert.ok(v && (v.kind === "filled" || (v.kind === "text" && v.text === "ada")), key + ": " + (v && v.kind));
      }
    }
  }
  t.diagnostic("ACCEPT A18 " + JSON.stringify(out));
});

// A17: D-439, remove-first rebuilds split across batches with a neutral identity (js-coder-4
// not_done 3, js-coder-5 not_done 1). Values are checked in world sb, as in A16.
test("A17 D-439: a remove-first neutral rebuild into the same parent (microtask, later task) never emits the value; a different parent or two fields still read", async (t) => {
  const secret = process.env.SB_VAULT_CANARY || "sbpw" + crypto.randomBytes(12).toString("hex");
  const o = await open("accept-d439.html");
  const OLD = ["rm-mic", "rm-task", "rm-other", "rm-two"];
  const NEW = ["plain-m", "plain-t"];
  const READ = { coupon: "WELCOME10", "first-n": "Ada", "last-n": "Lovelace" };
  await o.ev((ids) => {
    const d = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value");
    globalThis.__pwReads = {};
    Object.defineProperty(HTMLInputElement.prototype, "value", {
      configurable: true,
      enumerable: d.enumerable,
      get() {
        if (ids.includes(this.id)) globalThis.__pwReads[this.id] = (globalThis.__pwReads[this.id] || 0) + 1;
        return d.get.call(this);
      },
      set(v) {
        d.set.call(this, v);
      },
    });
  }, OLD.concat(NEW));
  for (const id of OLD) await o.page.fill("#" + id, secret);
  await o.ev(() => globalThis.__sb.index());
  for (const fn of ["removeMicro", "removeTask", "removeOther", "removeTwo"]) await o.page.evaluate((f) => globalThis[f](), fn);
  await o.page.waitForFunction(() => ["plain-m", "plain-t", "coupon", "first-n", "last-n"].every((i) => !!document.getElementById(i)), null, { timeout: 5000 });
  const r = await o.ev(async (a) => {
    const sb = globalThis.__sb;
    const out = [sb.walk({ dirty: true }), sb.index(), sb.text(), sb.frames()];
    const per = {};
    for (const id of a.ids) {
      const el = document.getElementById(id);
      const s = sb.state(el);
      const x = [s, sb.name(el), await sb.act(sb.id(el))];
      out.push(x);
      const v = s && s.value ? s.value : {};
      per[id] = { kind: v.kind, text: a.read.includes(id) ? v.text : undefined, value: JSON.stringify(x).includes(a.secret) };
    }
    out.push(sb.walk({}), sb.index(), sb.text());
    const blob = JSON.stringify(out);
    let frag = false;
    for (let i = 0; i + 8 <= a.secret.length; i += 2) if (blob.includes(a.secret.slice(i, i + 8))) frag = true;
    return { per, value: blob.includes(a.secret), frag, bytes: blob.length };
  }, { ids: NEW.concat(Object.keys(READ)), read: Object.keys(READ), secret });
  const reads = await o.ev(() => globalThis.__pwReads);
  const leaks = Array.from(new Set(Object.keys(reads).concat(NEW.filter((id) => r.per[id].value || r.per[id].kind !== "filled"))));
  t.diagnostic(`ACCEPT A17 secret len ${secret.length}${process.env.SB_VAULT_CANARY ? " (SB_VAULT_CANARY)" : ""}; kinds ${JSON.stringify(Object.fromEntries(Object.entries(r.per).map(([k, v]) => [k, v.kind])))}; ` +
    `value-getter reads ${JSON.stringify(reads)}; value in output ${r.value || r.frag} (${r.bytes} bytes); leaking fields: ${leaks.join(", ") || "none"}`);
  for (const [id, text] of Object.entries(READ)) assert.deepEqual({ kind: r.per[id].kind, text: r.per[id].text }, { kind: "text", text }, "negative " + id);
  assert.deepEqual(leaks, [], "fields whose value sb read or emitted: " + leaks.join(", "));
  assert.equal(r.value || r.frag, false, "the value or a fragment is in __sb output");
});

// A19 (tester-9, round 4): A10 now waits out D-442's 2 s window before its swaps, so its leak
// checks no longer run on a swap made while the parser-inserted fields are still remembered.
// This runs A10's site handlers (show toggle, rebuild, wrapper, re-append, masking dropped) as
// soon as the fields are filled after load, with the leak checks only (the Nickname over-mark
// inside the window is D-442 (3)'s accepted cost and is A18's). Values are compared inside
// world sb; only kinds, counts and booleans come back.
test("A19 D-442, D-443: A10's password rebuilds within 2 s of load never read or emit the value", async (t) => {
  const secret = process.env.SB_VAULT_CANARY || "sbpw" + crypto.randomBytes(12).toString("hex");
  const o = await open("accept-pw.html", { base: null });
  const OLD = ["p1", "p2", "m1", "m2", "ac", "nm", "r1", "w-in", "r3"];
  const NEW = ["plain-copy", "wrap-copy", "appended-copy"];
  await o.ev((ids) => {
    const d = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value");
    globalThis.__pwReads = {};
    Object.defineProperty(HTMLInputElement.prototype, "value", {
      configurable: true,
      enumerable: d.enumerable,
      get() {
        if (ids.includes(this.id)) globalThis.__pwReads[this.id] = (globalThis.__pwReads[this.id] || 0) + 1;
        return d.get.call(this);
      },
      set(v) {
        d.set.call(this, v);
      },
    });
  }, OLD.concat(NEW));
  await o.ev(() => globalThis.__sb.walk({ base: 1 }));
  for (const id of OLD) await o.page.fill("#" + id, secret);
  await o.ev(() => globalThis.__sb.index());
  await o.page.click("#btn-show");
  await o.page.click("#btn-rebuild");
  await o.page.click("#btn-wrap");
  await o.page.click("#btn-append");
  await o.page.evaluate(() => {
    document.getElementById("m1").classList.remove("masked");
    document.getElementById("m2").removeAttribute("style");
    document.getElementById("ac").removeAttribute("autocomplete");
  });
  // an upper bound on the last swap's time; every field was inserted after time 0
  const at = await o.page.evaluate(() => performance.now());
  const facts = await o.page.evaluate((n) => n.map((i) => (document.getElementById(i) ? document.getElementById(i).value.length : -1)), NEW);
  assert.deepEqual(facts, NEW.map(() => secret.length), "the rebuilt fields carry the value");
  const r = await o.ev(async (a) => {
    const sb = globalThis.__sb;
    const out = [sb.walk({ dirty: true }), sb.index(), sb.text(), sb.text("f1"), sb.frames(),
      sb.fingerprint("textbox \"Password one\" textbox \"Shown field\"")];
    const kinds = {};
    for (const id of a.ids) {
      const el = document.getElementById(id);
      const s = sb.state(el);
      out.push([s, sb.name(el), await sb.act(sb.id(el))]);
      kinds[id] = s && s.value ? s.value.kind + ("text" in s.value ? "+text" : "") : String(s && s.value);
    }
    out.push(await sb.settle({ cap: 400 }), sb.walk({}), sb.index(), sb.text());
    const blob = JSON.stringify(out);
    let frag = false;
    for (let i = 0; i + 8 <= a.secret.length; i += 2) if (blob.includes(a.secret.slice(i, i + 8))) frag = true;
    return { kinds, value: blob.includes(a.secret), frag, bytes: blob.length };
  }, { ids: ["p1", "p2", "m1", "m2", "ac", "nm"].concat(NEW), secret });
  const reads = await o.ev(() => globalThis.__pwReads);
  t.diagnostic(`ACCEPT A19 secret len ${secret.length}${process.env.SB_VAULT_CANARY ? " (SB_VAULT_CANARY)" : ""}; last swap by ${Math.round(at)} ms ` +
    `(${at <= 2000 ? "inside" : "outside"} the 2 s window); kinds ${JSON.stringify(r.kinds)}; value-getter reads ${JSON.stringify(reads)}; ` +
    `value in output ${r.value || r.frag} (${r.bytes} bytes)`);
  assert.deepEqual(reads, {}, "world sb called the value getter of a sensitive field: " + JSON.stringify(reads));
  assert.equal(r.value || r.frag, false, "the value or a fragment is in __sb output");
  for (const [id, k] of Object.entries(r.kinds)) assert.equal(k, "filled", "kind of " + id);
});

// A20 (tester-9, round 4): D-443 (1) and (4), independent of the coder's O1..O3. An add-first
// show toggle with a JS-timed exit: the neutral copy is read while the password is still in the
// page (right after the click, and right after settle() in the same evaluate) and after the
// password goes. Controls: a field added next to a password that stays is masked, then reads
// after 2 s; a field added to a box with no password reads at once. Values compared in world sb.
test("A20 D-443: a copy read while the password is still there never reads the value; a field beside a live password is masked for 2 s only, one elsewhere reads at once", async (t) => {
  const secret = process.env.SB_VAULT_CANARY || "sbpw" + crypto.randomBytes(12).toString("hex");
  const o = await open("accept-d443.html");
  await o.ev((ids) => {
    const d = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value");
    globalThis.__pwReads = {};
    Object.defineProperty(HTMLInputElement.prototype, "value", {
      configurable: true,
      enumerable: d.enumerable,
      get() {
        if (ids.includes(this.id)) globalThis.__pwReads[this.id] = (globalThis.__pwReads[this.id] || 0) + 1;
        return d.get.call(this);
      },
      set(v) {
        d.set.call(this, v);
      },
    });
  }, ["pw", "pw2", "copy-late"]);
  await o.page.fill("#pw", secret);
  await o.page.fill("#pw2", secret);
  await o.ev(() => globalThis.__sb.index());
  // one read in world sb: kinds (text only for the non-secret controls), whether the value or
  // a fragment is anywhere in the output, and whether #pw is still in the page
  const read = (settleFirst) => o.ev(async (a) => {
    const sb = globalThis.__sb;
    const out = [];
    if (a.settleFirst) out.push(await sb.settle({ cap: 3000 }));
    const present = !!document.getElementById("pw");
    out.push(sb.walk({ dirty: true }), sb.index(), sb.text(), sb.text("f1"), sb.frames(),
      sb.fingerprint("textbox \"Password\" textbox \"Visible entry\""));
    const kinds = {};
    for (const id of ["copy-late", "pw2", "beside", "away", "email"]) {
      const el = document.getElementById(id);
      if (!el) continue;
      const s = sb.state(el);
      out.push([s, sb.name(el), await sb.act(sb.id(el))]);
      const v = s && s.value ? s.value : {};
      kinds[id] = { kind: v.kind, text: a.plain.includes(id) ? v.text : ("text" in v ? "<has text>" : undefined) };
    }
    const blob = JSON.stringify(out);
    let frag = false;
    for (let i = 0; i + 8 <= a.secret.length; i += 2) if (blob.includes(a.secret.slice(i, i + 8))) frag = true;
    return { present, kinds, leak: blob.includes(a.secret) || frag, bytes: blob.length };
  }, { settleFirst, secret, plain: ["beside", "away", "email"] });
  await o.page.click("#show");
  const mid = await read(false);
  const same = await read(true);
  await o.page.waitForFunction(() => !document.getElementById("pw"), null, { timeout: 5000 });
  const gone = await read(false);
  await o.page.click("#beside-btn");
  await o.page.click("#away-btn");
  const addedAt = await o.page.evaluate(() => performance.now());
  const near = await read(false);
  await pastBornWindow(o, addedAt);
  const late = await read(false);
  const reads = await o.ev(() => globalThis.__pwReads);
  const all = { mid, same, gone, near, late };
  t.diagnostic(`ACCEPT A20 secret len ${secret.length}${process.env.SB_VAULT_CANARY ? " (SB_VAULT_CANARY)" : ""}; ` +
    Object.entries(all).map(([k, r]) => `${k}: pw ${r.present ? "present" : "gone"} copy ${r.kinds["copy-late"] && r.kinds["copy-late"].kind} ` +
      `beside ${r.kinds.beside ? r.kinds.beside.kind : "-"} away ${r.kinds.away ? r.kinds.away.kind : "-"} leak ${r.leak}`).join("; ") +
    `; value-getter reads ${JSON.stringify(reads)}`);
  assert.equal(mid.present, true, "the first read must overlap the password (the exit is 700 ms)");
  assert.deepEqual(reads, {}, "world sb called the value getter of the password or its copy: " + JSON.stringify(reads));
  for (const [k, r] of Object.entries(all)) {
    assert.equal(r.leak, false, k + ": the value or a fragment is in __sb output");
    assert.deepEqual(r.kinds["copy-late"], { kind: "filled" }, k + ": copy");
    assert.deepEqual(r.kinds.pw2, { kind: "filled" }, k + ": pw2");
    assert.deepEqual(r.kinds.email, { kind: "text", text: "ada@example.org" }, k + ": email (untouched, read at once)");
  }
  // D-443 (1): masked beside a live password; D-443 (4): for at most 2 s; elsewhere never
  assert.ok(near.kinds.beside && ["empty", "filled"].includes(near.kinds.beside.kind) && near.kinds.beside.text === undefined, "beside, within 2 s: " + (near.kinds.beside && near.kinds.beside.kind));
  assert.deepEqual(near.kinds.away, { kind: "text", text: "away text" }, "away, at once");
  assert.deepEqual(late.kinds.beside, { kind: "text", text: "beside text" }, "beside, after 2 s");
  assert.deepEqual(late.kinds.away, { kind: "text", text: "away text" }, "away, after 2 s");
});

// A21 (tester-9, round 4): review-T-19 round 8 Blocking (Y7 twin, Y6 clone and hide), D-444 (1).
// The usual flow: type the password, click Show, settle(), read, and read again 2.1 s later.
// The twin is in the page from load and only its display flips; the clone is added and the
// password is hidden, not removed. Values compared in world sb; only kinds and booleans return.
test("A21 D-444 (1): visibility-swap show toggles (a synced twin, clone and hide) never read or emit the value", async (t) => {
  const secret = process.env.SB_VAULT_CANARY || "sbpw" + crypto.randomBytes(12).toString("hex");
  const o = await open("accept-d444.html");
  const IDS = ["pw-t", "twin-t", "pw-c", "clone-c"];
  await o.ev((ids) => {
    const d = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value");
    globalThis.__pwReads = {};
    Object.defineProperty(HTMLInputElement.prototype, "value", {
      configurable: true,
      enumerable: d.enumerable,
      get() {
        if (ids.includes(this.id)) globalThis.__pwReads[this.id] = (globalThis.__pwReads[this.id] || 0) + 1;
        return d.get.call(this);
      },
      set(v) {
        d.set.call(this, v);
      },
    });
  }, IDS);
  await o.page.fill("#pw-t", secret);
  await o.page.fill("#pw-c", secret);
  const synced = await o.page.evaluate(() => document.getElementById("twin-t").value.length);
  assert.equal(synced, secret.length, "the twin carries the value");
  await o.ev(() => globalThis.__sb.index());
  const read = (settleFirst) => o.ev(async (a) => {
    const sb = globalThis.__sb;
    const out = [];
    if (a.settleFirst) out.push(await sb.settle({ cap: 3000 }));
    out.push(sb.walk({ dirty: true }), sb.index(), sb.text(), sb.text("f1"), sb.text("f2"), sb.frames(),
      sb.fingerprint("textbox \"Visible entry\" textbox \"Shown entry\""));
    const kinds = {};
    for (const id of a.ids.concat(["user"])) {
      const el = document.getElementById(id);
      if (!el) continue;
      const s = sb.state(el);
      out.push([s, sb.name(el), await sb.act(sb.id(el))]);
      const v = s && s.value ? s.value : {};
      kinds[id] = id === "user" ? v.text : v.kind + ("text" in v ? "+text" : "");
    }
    const blob = JSON.stringify(out);
    let frag = false;
    for (let i = 0; i + 8 <= a.secret.length; i += 2) if (blob.includes(a.secret.slice(i, i + 8))) frag = true;
    return { kinds, leak: blob.includes(a.secret) || frag };
  }, { settleFirst, secret, ids: IDS });
  await o.page.click("#show-t");
  await o.page.click("#show-c");
  const same = await read(true);
  const next = await read(false);
  await pastBornWindow(o, await o.page.evaluate(() => performance.now()));
  const late = await read(false);
  const reads = await o.ev(() => globalThis.__pwReads);
  const all = { same, next, late };
  t.diagnostic(`ACCEPT A21 secret len ${secret.length}${process.env.SB_VAULT_CANARY ? " (SB_VAULT_CANARY)" : ""}; ` +
    Object.entries(all).map(([k, r]) => `${k}: twin ${r.kinds["twin-t"]} clone ${r.kinds["clone-c"]} leak ${r.leak}`).join("; ") +
    `; value-getter reads ${JSON.stringify(reads)}`);
  assert.deepEqual(reads, {}, "world sb called the value getter of a password, its twin or its clone: " + JSON.stringify(reads));
  for (const [k, r] of Object.entries(all)) {
    assert.equal(r.leak, false, k + ": the value or a fragment is in __sb output");
    for (const id of ["twin-t", "clone-c"]) assert.ok(["empty", "filled"].includes(r.kinds[id]), k + ": " + id + " " + r.kinds[id]);
    assert.equal(r.kinds.user, "ada", k + ": user (control) reads its text");
  }
});

// A22 (tester-10, round 5): review-T-19 round 8 Y3, Y3x, Y3d (D-444 (2)) and Y5, Y5v (D-444 (3))
// in Chromium. One page load per case; type the password, click Show, read as soon as the copy
// is in the page (mid), after settle() in the same evaluate (same), again (next), and once the
// page clock is 2.05 s past the last change (late). Values compared in world sb; only kinds
// and booleans return.
test("A22 D-444 (2)(3): a password moved or re-attached in its copy's batch, or replaced by a new password with its key, never lets the copy read or emit the value", async (t) => {
  const secret = process.env.SB_VAULT_CANARY || "sbpw" + crypto.randomBytes(12).toString("hex");
  const CASES = [
    // [case, the old password leaves the page later (wait for it before the late read)]
    ["y3", true],
    ["y3x", true],
    ["y3d", true],
    ["y5", false],
    ["y5v", false],
  ];
  const lines = [];
  const fails = [];
  for (const [k, removedLater] of CASES) {
    const o = await open("accept-d444b.html");
    const IDS = ["pw-" + k, "copy-" + k, "pw2-" + k];
    await o.ev((ids) => {
      const d = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value");
      globalThis.__pwReads = {};
      Object.defineProperty(HTMLInputElement.prototype, "value", {
        configurable: true,
        enumerable: d.enumerable,
        get() {
          if (ids.includes(this.id)) globalThis.__pwReads[this.id] = (globalThis.__pwReads[this.id] || 0) + 1;
          return d.get.call(this);
        },
        set(v) {
          d.set.call(this, v);
        },
      });
    }, IDS);
    await o.page.fill("#pw-" + k, secret);
    await o.ev(() => globalThis.__sb.index());
    const read = (settleFirst) => o.ev(async (a) => {
      const sb = globalThis.__sb;
      const out = [];
      if (a.settleFirst) out.push(await sb.settle({ cap: 3000 }));
      const present = !!document.getElementById(a.ids[0]);
      out.push(sb.walk({ dirty: true }), sb.index(), sb.text(), sb.text("f-" + a.k), sb.frames(),
        sb.fingerprint("textbox \"Shown entry " + a.k + "\""));
      const kinds = {};
      for (const id of a.ids.concat(["user"])) {
        const el = document.getElementById(id);
        if (!el) continue;
        const s = sb.state(el);
        out.push([s, sb.name(el), await sb.act(sb.id(el))]);
        const v = s && s.value ? s.value : {};
        kinds[id] = id === "user" ? v.text : v.kind + ("text" in v ? "+text" : "");
      }
      const blob = JSON.stringify(out);
      let frag = false;
      for (let i = 0; i + 8 <= a.secret.length; i += 2) if (blob.includes(a.secret.slice(i, i + 8))) frag = true;
      return { present, kinds, leak: blob.includes(a.secret) || frag };
    }, { settleFirst, secret, ids: IDS, k });
    await o.page.click("#show-" + k, { noWaitAfter: true });
    await o.page.waitForFunction((id) => !!document.getElementById(id), "copy-" + k, { polling: "raf", timeout: 5000 });
    const mid = await read(false);
    const same = await read(true);
    const next = await read(false);
    if (removedLater) await o.page.waitForFunction((id) => !document.getElementById(id), "pw-" + k, { polling: "raf", timeout: 5000 });
    await pastBornWindow(o, await o.page.evaluate(() => performance.now()));
    const late = await read(false);
    const reads = await o.ev(() => globalThis.__pwReads);
    const all = { mid, same, next, late };
    lines.push(`${k}: ` + Object.entries(all).map(([w, r]) => `${w} pw ${r.present ? "present" : "gone"} copy ${r.kinds["copy-" + k]}` +
      (r.kinds["pw2-" + k] ? ` pw2 ${r.kinds["pw2-" + k]}` : "") + ` leak ${r.leak}`).join(", ") + `, getter ${JSON.stringify(reads)}`);
    if (Object.keys(reads).length) fails.push(`${k}: world sb called a value getter ${JSON.stringify(reads)}`);
    for (const [w, r] of Object.entries(all)) {
      if (r.leak) fails.push(`${k} ${w}: the value or a fragment is in __sb output`);
      if (!["empty", "filled"].includes(r.kinds["copy-" + k])) fails.push(`${k} ${w}: copy ${r.kinds["copy-" + k]}`);
      if (r.kinds["pw2-" + k] && r.kinds["pw2-" + k].endsWith("+text")) fails.push(`${k} ${w}: new password ${r.kinds["pw2-" + k]}`);
      if (r.kinds.user !== "ada") fails.push(`${k} ${w}: user (control) ${r.kinds.user}`);
    }
    // the moves must overlap the password, or the case does not test D-444 (2)
    if (removedLater && !mid.present) fails.push(`${k}: the mid read did not overlap the password`);
  }
  t.diagnostic(`ACCEPT A22 secret len ${secret.length}${process.env.SB_VAULT_CANARY ? " (SB_VAULT_CANARY)" : ""}; ` + lines.join("; "));
  assert.deepEqual(fails, []);
});

// A23 (tester-11, round 6): D-445 (1)(2), review-T-19 round 9 Z7v and Z4, in a tester fixture
// (bench/pages/accept-d445.html). v: a Vue v-show component (div.field > label + div.control >
// input + p.help) rendered twice, Show flips the components' display, so the password and its
// synced plain twin sit two levels below their wrapper. o: Show moves the password off the top
// of the page (absolute, top -9999px, full-size box) and the twin back in; no display change.
// One page load per case: type the password, click Show, settle() and read in one evaluate
// (same), read again (next), and once the page clock is 2.05 s past the click (late). Values
// are compared in world sb; only kinds, counts and booleans return.
test("A23 D-445 (1)(2): a Vue v-show component twin and an off-screen password never let the twin read or emit the value", async (t) => {
  const secret = process.env.SB_VAULT_CANARY || "sbpw" + crypto.randomBytes(12).toString("hex");
  const lines = [];
  const fails = [];
  for (const k of ["v", "o"]) {
    const o = await open("accept-d445.html");
    const IDS = ["pw-" + k, "twin-" + k];
    await o.page.fill("#pw-" + k, secret);
    await o.page.fill("#user", "ada");
    const synced = await o.page.evaluate((id) => document.getElementById(id).value.length, "twin-" + k);
    if (synced !== secret.length) fails.push(`${k}: the twin does not carry the value (${synced})`);
    await o.ev(() => globalThis.__sb.index());
    await o.ev((ids) => {
      const d = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value");
      globalThis.__pwReads = {};
      Object.defineProperty(HTMLInputElement.prototype, "value", {
        configurable: true,
        enumerable: d.enumerable,
        get() {
          if (ids.includes(this.id)) globalThis.__pwReads[this.id] = (globalThis.__pwReads[this.id] || 0) + 1;
          return d.get.call(this);
        },
        set(v) {
          d.set.call(this, v);
        },
      });
    }, IDS);
    const read = (settleFirst) => o.ev(async (a) => {
      const sb = globalThis.__sb;
      const out = [];
      let settled = null;
      if (a.settleFirst) {
        const s = await sb.settle({ cap: 3000 });
        settled = !!(s && s.settled);
        out.push(s);
      }
      const pw = document.getElementById(a.ids[0]);
      const r = pw.getBoundingClientRect();
      // the shape really happened: v hides the password's component, o puts it above the page
      const hidden = a.k === "v" ? pw.offsetParent === null : r.bottom + window.scrollY < 0;
      out.push(sb.walk({ dirty: true }), sb.index(), sb.text(), sb.text("f" + a.k), sb.frames(),
        sb.fingerprint("textbox \"Password\" textbox \"Passcode shown\""));
      const kinds = {};
      for (const id of a.ids.concat(["user"])) {
        const el = document.getElementById(id);
        const s = sb.state(el);
        out.push([s, sb.name(el), await sb.act(sb.id(el))]);
        const v = s && s.value ? s.value : {};
        kinds[id] = id === "user" ? v.text : v.kind + ("text" in v ? "+text" : "");
      }
      const blob = JSON.stringify(out);
      let frag = false;
      for (let i = 0; i + 8 <= a.secret.length; i += 2) if (blob.includes(a.secret.slice(i, i + 8))) frag = true;
      return { settled, hidden, kinds, leak: blob.includes(a.secret) || frag };
    }, { settleFirst, secret, ids: IDS, k });
    const t0 = await o.page.evaluate(() => performance.now());
    await o.page.click("#show-" + k, { noWaitAfter: true });
    const same = await read(true);
    const next = await read(false);
    await pastBornWindow(o, t0);
    const late = await read(false);
    const reads = await o.ev(() => globalThis.__pwReads);
    const all = { same, next, late };
    lines.push(`${k}: settled ${same.settled}, ` + Object.entries(all).map(([w, r]) =>
      `${w} pw ${r.hidden ? "hidden" : "SHOWN"} twin ${r.kinds["twin-" + k]} leak ${r.leak}`).join(", ") + `, getter ${JSON.stringify(reads)}`);
    if (same.settled !== true) fails.push(`${k}: settle() did not settle`);
    if (Object.keys(reads).length) fails.push(`${k}: world sb called a value getter ${JSON.stringify(reads)}`);
    for (const [w, r] of Object.entries(all)) {
      if (!r.hidden) fails.push(`${k} ${w}: the password is not hidden, the case does not test D-445`);
      if (r.leak) fails.push(`${k} ${w}: the value or a fragment is in __sb output`);
      if (!["empty", "filled"].includes(r.kinds["twin-" + k])) fails.push(`${k} ${w}: twin ${r.kinds["twin-" + k]}`);
      if (r.kinds["pw-" + k] && r.kinds["pw-" + k].endsWith("+text")) fails.push(`${k} ${w}: password ${r.kinds["pw-" + k]}`);
      if (r.kinds.user !== "ada") fails.push(`${k} ${w}: user (control) ${r.kinds.user}`);
    }
  }
  t.diagnostic(`ACCEPT A23 secret len ${secret.length}${process.env.SB_VAULT_CANARY ? " (SB_VAULT_CANARY)" : ""}; ` + lines.join("; "));
  assert.deepEqual(fails, []);
});
