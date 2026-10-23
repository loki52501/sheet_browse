// Layout-dependent parts of injected.js in real Chromium (D-401): visibility, occlusion,
// stability, act timings, settle cap, frames. injected.js is installed exactly as sb-driver
// installs it (Page.addScriptToEvaluateOnNewDocument, world "sb") and called in that world.
// docs/05 acceptance 1, the in-page parts of 2 and 5, and the index side of 6.
"use strict";

const { test, before, after } = require("node:test");
const assert = require("node:assert/strict");
const { serve, launch, openSb, byName, fill } = require("./helpers");

let browser;
let A; // origin 1
let B; // origin 2 (same site, other port: a real cross-origin frame)
const opened = [];

before(async () => {
  [A, B] = await Promise.all([serve(), serve()]);
  browser = await launch();
});
after(async () => {
  for (const o of opened) await o.close().catch(() => undefined);
  if (browser) await browser.close();
  if (A) await A.close();
  if (B) await B.close();
});

async function open(name, opts) {
  const o = await openSb(browser, A.origin + "/" + name, opts);
  opened.push(o);
  return o;
}

// Run __sb.act(id) in the page and time it there (the < 50 ms budget is the in-page call).
const timedAct = (o, id) => o.ev(async (i) => {
  const t = performance.now();
  const r = await globalThis.__sb.act(i);
  return { r, ms: performance.now() - t };
}, id);

test("isolated world: __sb exists only in world sb, frozen", async () => {
  const o = await open("register.html");
  assert.equal(await o.page.evaluate(() => typeof window.__sb), "undefined");
  assert.equal(await o.ev("typeof globalThis.__sb.walk"), "function");
  assert.equal(await o.ev("Object.isFrozen(globalThis.__sb)"), true);
  // a second install (evaluate into an already-loaded document) is a no-op
  const { SRC } = require("./helpers");
  await o.ev(SRC);
  assert.equal(await o.ev("globalThis.__sb.walk({base: 1}).count > 0"), true);
});

test("register.html index (acceptance 6, index side): names, flags, one CHALLENGE+OPAQUE row, overlay, hidden promo absent", async () => {
  const o = await open("register.html");
  await o.ev(() => globalThis.__sb.walk({ base: 1 }));
  const rows = fill(await o.ev(() => globalThis.__sb.index()));
  const email = rows.filter((r) => r.name === "Email");
  assert.equal(email.length, 2);
  assert.deepEqual(email.map((r) => r.form), ["f1", "f2"]);
  const pw = byName(rows, "Password");
  assert.deepEqual(pw.state.value, { kind: "empty" });
  assert.equal(pw.state.input_type, "password");
  assert.ok(byName(rows, "Full name", "textbox"));
  assert.deepEqual(byName(rows, "Plan", "combobox").state.options, ["Basic", "Plus", "Pro"]);
  assert.equal(byName(rows, "I agree to the Terms of Service", "checkbox").state.checked, false);
  const reg = byName(rows, "Register", "button");
  assert.ok(reg.flags.submit && reg.state.submitish && reg.form === "f1");
  // one row for the reCAPTCHA container and its sandboxed (opaque-origin) iframe
  const ch = rows.filter((r) => r.flags.challenge);
  assert.equal(ch.length, 1);
  assert.equal(ch[0].role, "iframe");
  assert.equal(ch[0].name, "reCAPTCHA");
  assert.equal(ch[0].flags.opaque, true);
  // cookie banner: OVERLAY row, after the footer in document order
  const ov = rows.filter((r) => r.flags.overlay);
  assert.equal(ov.length, 1);
  assert.equal(ov[0].name, "Cookie consent");
  assert.equal(ov[0].role, "region");
  assert.ok(ov[0].id > byName(rows, "Subscribe").id);
  assert.ok(byName(rows, "Accept all", "button").landmark === "region");
  // the banner covers the bottom of the viewport, where Register sits
  assert.equal(reg.state.covered, true);
  assert.equal(byName(rows, "Email").state.covered, false);
  // hidden promo: absent with its subtree
  assert.ok(!rows.some((r) => /offer/i.test(r.name)));
  // geometry: every visible row has a box in viewport coordinates
  for (const r of rows) {
    assert.ok(r.box, "box for " + r.name);
    if (r.state.visible) assert.ok(r.box.w > 0 && r.box.h > 0, r.name);
  }
  // nearby text from real geometry: the paragraph above the form
  assert.equal(byName(rows, "Email").heading, "Create your account");
});

test("identity (acceptance 1): same ids across 3 walks with unrelated DOM changes; removed -> gone", async () => {
  const o = await open("register.html");
  await o.ev(() => globalThis.__sb.walk({ base: 1000 }));
  const first = fill(await o.ev(() => globalThis.__sb.index()));
  assert.equal(first[0].id, 1000);
  const key = (r) => r.role + ":" + r.name + ":" + (r.form || "");
  const want = new Map(first.map((r) => [key(r), r.id]));
  const check = (rows) => {
    for (const r of first) {
      const x = rows.find((y) => key(y) === key(r));
      assert.ok(x, "still there: " + key(r));
      assert.equal(x.id, want.get(key(r)), key(r));
    }
  };
  // site changes run in the main world, as the page's own scripts would
  await o.page.evaluate(() => {
    const p = document.createElement("p");
    p.textContent = "Inserted above the form";
    document.querySelector("main").prepend(p);
  });
  await o.ev(() => globalThis.__sb.walk({ dirty: true }));
  check(fill(await o.ev(() => globalThis.__sb.index())));
  await o.page.evaluate(() => {
    document.querySelector("h1").classList.add("big");
    document.querySelector("footer").append(Object.assign(document.createElement("button"), { textContent: "Late button" }));
  });
  await o.ev(() => globalThis.__sb.walk({ dirty: true }));
  const second = fill(await o.ev(() => globalThis.__sb.index()));
  check(second);
  const late = byName(second, "Late button");
  assert.ok(late.id > Math.max(...first.map((r) => r.id)));
  await o.ev(() => globalThis.__sb.walk({}));
  check(fill(await o.ev(() => globalThis.__sb.index())));
  const emailId = byName(first, "Email").id;
  await o.page.evaluate(() => document.getElementById("email").remove());
  assert.deepEqual(await o.ev((i) => globalThis.__sb.resolve(i), emailId), { why: "gone" });
  assert.deepEqual((await timedAct(o, emailId)).r, { why: "gone" });
  assert.deepEqual(await o.ev(() => globalThis.__sb.resolve(3)), { why: "stale" });
});

test("act (acceptance 2, in page): covered < 50 ms, disabled, moving, retry after the cover is removed", async (t) => {
  const o = await open("overlay.html");
  await o.ev(() => globalThis.__sb.walk({ base: 1 }));
  const rows = fill(await o.ev(() => globalThis.__sb.index()));
  const buy = byName(rows, "Buy now");
  const later = byName(rows, "Pay later");
  const mover = byName(rows, "Moving");
  const accept = byName(rows, "Accept");
  const cookie = rows.find((r) => r.flags.overlay);
  assert.ok(cookie, "the cookie layer is an OVERLAY row");
  assert.equal(buy.state.covered, true);
  assert.equal(later.state.enabled, false);

  const c = await timedAct(o, buy.id);
  assert.match(c.r.why, /^covered by div\.cookie \(OVERLAY #\d+\)$/);
  assert.equal(c.r.why, `covered by div.cookie (OVERLAY #${cookie.id})`);
  assert.ok(c.ms < 50, `covered answered in ${c.ms.toFixed(1)} ms`);
  t.diagnostic(`act covered: ${c.ms.toFixed(1)} ms`);

  const d = await timedAct(o, later.id);
  assert.deepEqual(d.r, { why: "disabled" });
  assert.ok(d.ms < 50, `disabled answered in ${d.ms.toFixed(1)} ms`);
  t.diagnostic(`act disabled: ${d.ms.toFixed(1)} ms`);

  const m = await timedAct(o, mover.id);
  assert.deepEqual(m.r, { why: "not stable (moving)" });

  // real input at the point act returned removes the overlay; the retry then succeeds
  const a = await timedAct(o, accept.id);
  assert.equal(typeof a.r.x, "number");
  await o.page.mouse.click(a.r.x, a.r.y);
  const ok = await timedAct(o, buy.id);
  assert.equal(typeof ok.r.x, "number", JSON.stringify(ok.r));
  await o.page.mouse.click(ok.r.x, ok.r.y);
  assert.equal(await o.page.textContent("#log"), "bought");
  const after = fill(await o.ev(() => globalThis.__sb.index()));
  assert.equal(byName(after, "Buy now").state.covered, false);
  assert.ok(!after.some((r) => r.flags.overlay));

  // below the fold: act scrolls it into view and returns a point inside the viewport
  const below = byName(after, "Below the fold");
  const b = await timedAct(o, below.id);
  assert.ok(b.r.y > 0 && b.r.y < 720, JSON.stringify(b.r));

  // gone
  const gone = byName(after, "Gone soon");
  await o.page.evaluate(() => document.getElementById("gone").remove());
  assert.deepEqual((await timedAct(o, gone.id)).r, { why: "gone" });
});

test("act on a CHALLENGE row is refused", async () => {
  const o = await open("register.html");
  const rows = fill(await o.ev(() => globalThis.__sb.index()));
  const ch = rows.find((r) => r.flags.challenge);
  assert.deepEqual((await timedAct(o, ch.id)).r, { why: "challenge" });
});

test("settle (acceptance 5): SPA rendering 400 ms after load is non-empty after settle", async () => {
  const o = await open("spa.html");
  const empty = fill(await o.ev(() => globalThis.__sb.index()));
  assert.equal(empty.length, 0, "nothing rendered yet at load");
  const s = await o.ev(async () => {
    const t = performance.now();
    const r = await globalThis.__sb.settle();
    return { r, ms: performance.now() - t };
  });
  assert.equal(s.r.settled, true);
  assert.ok(s.r.changed > 0);
  assert.ok(s.ms < 3000, `settled in ${s.ms.toFixed(0)} ms`);
  const rows = fill(await o.ev(() => globalThis.__sb.index()));
  assert.ok(byName(rows, "Dashboard", "heading"));
  assert.ok(byName(rows, "Search projects", "textbox"));
  const text = await o.ev(() => globalThis.__sb.text());
  assert.ok(text.some((b) => b.spans.some((sp) => /3 open projects/.test(sp.text))));
  // the fade-in animation on the changed node had finished
  assert.equal(await o.ev(() => document.getAnimations().filter((a) => a.playState === "running").length), 0);
});

test("settle cap is never exceeded on a page that never stops mutating", async (t) => {
  const o = await open("busy.html");
  const run = (opts) => o.ev(async (op) => {
    const t = performance.now();
    const r = await globalThis.__sb.settle(op || undefined);
    return { r, ms: performance.now() - t };
  }, opts);
  const full = await run(null);
  assert.equal(full.r.settled, false);
  assert.ok(full.r.changed > 10);
  assert.ok(full.ms >= 2990 && full.ms < 3100, `default cap took ${full.ms.toFixed(0)} ms`);
  t.diagnostic(`settle default cap: ${full.ms.toFixed(0)} ms`);
  const short = await run({ cap: 500 });
  assert.equal(short.r.settled, false);
  assert.ok(short.ms >= 495 && short.ms < 600, `cap 500 took ${short.ms.toFixed(0)} ms`);
});

test("settle on a quiet page resolves after the quiet window, not the cap", async () => {
  const o = await open("register.html");
  const s = await o.ev(async () => {
    const t = performance.now();
    const r = await globalThis.__sb.settle();
    return { r, ms: performance.now() - t };
  });
  assert.equal(s.r.settled, true);
  assert.equal(s.r.changed, 0);
  assert.ok(s.ms >= 145 && s.ms < 400, `quiet settle took ${s.ms.toFixed(0)} ms`);
});

test("frames: same-origin child has its own __sb; cross-origin and captcha frames are one OPAQUE row", async () => {
  const o = await open("frames.html?xo=" + encodeURIComponent(B.origin), { waitUntil: "load" });
  const rows = fill(await o.ev(() => globalThis.__sb.index()));
  const frs = rows.filter((r) => r.role === "iframe");
  assert.equal(frs.length, 3, JSON.stringify(rows.map((r) => r.role + ":" + r.name)));
  const same = byName(rows, "Same-origin child");
  const cross = byName(rows, "Cross-origin child");
  const cap = rows.find((r) => r.flags.challenge);
  assert.ok(!same.flags.opaque);
  assert.equal(cross.flags.opaque, true);
  assert.equal(cap.role, "iframe");
  assert.equal(cap.flags.opaque, true);
  assert.equal(rows.filter((r) => r.flags.challenge).length, 1);
  // the parent never emits rows from inside any child frame
  assert.ok(!byName(rows, "Nickname"));
  const list = await o.ev(() => globalThis.__sb.frames());
  assert.equal(list.length, 4);
  assert.deepEqual(list.slice(1).map((f) => f.opaque), [false, true, true]);
  assert.ok(list[2].url.startsWith(B.origin));
  // child frames: each has its own __sb in world sb
  const tree = await o.frames();
  const sameF = tree.find((f) => f.url === A.origin + "/frame-child.html");
  const crossF = tree.find((f) => f.url === B.origin + "/frame-child.html");
  const capF = tree.find((f) => f.url.includes("widget=hcaptcha"));
  assert.ok(sameF && crossF && capF, JSON.stringify(tree));
  const childRows = fill(await o.ev(() => globalThis.__sb.index(), undefined, sameF.id));
  assert.ok(byName(childRows, "Nickname", "textbox"));
  assert.equal(byName(childRows, "Save nickname").form, "f1");
  const crossRows = fill(await o.ev(() => globalThis.__sb.index(), undefined, crossF.id));
  assert.ok(byName(crossRows, "Nickname"));
  // inside a challenge frame the page API reads nothing (D-5)
  assert.deepEqual(await o.ev(() => globalThis.__sb.index(), undefined, capF.id), []);
  assert.deepEqual(await o.ev(() => globalThis.__sb.text(), undefined, capF.id), []);
});

test("popup.html: opens_new_tab from target=_blank and onclick window.open", async () => {
  const o = await open("popup.html");
  const rows = fill(await o.ev(() => globalThis.__sb.index()));
  assert.equal(byName(rows, "Open register in a new tab", "link").state.opens_new_tab, true);
  assert.equal(byName(rows, "Open popup window", "button").state.opens_new_tab, true);
  assert.equal(byName(rows, "Open popup from a listener").state.opens_new_tab, false);
  assert.equal(byName(rows, "Does nothing").state.opens_new_tab, false);
  assert.equal(byName(rows, "Register in this tab", "link").state.opens_new_tab, false);
});

test("dialog.html: dialog and download controls are rows", async () => {
  const o = await open("dialog.html");
  const rows = fill(await o.ev(() => globalThis.__sb.index()));
  for (const n of ["Show alert", "Delete item", "Ask name"]) assert.ok(byName(rows, n, "button"), n);
  assert.ok(byName(rows, "Download notes", "link"));
});

test("controls.html with layout: name ladder incl. nearby geometry, shadow DOM rows", async () => {
  const o = await open("controls.html");
  const res = await o.ev(() => {
    const sb = globalThis.__sb;
    sb.walk({ base: 1 });
    const rows = sb.index();
    const els = Array.from(document.querySelectorAll("[data-expect]"));
    els.push(...document.getElementById("host").shadowRoot.querySelectorAll("[data-expect]"));
    return els.map((el) => {
      const id = sb.id(el);
      const r = rows.find((x) => x.id === id);
      return { want: el.getAttribute("data-expect"), got: r ? r.name : null };
    });
  });
  for (const { want, got } of res) assert.equal(got, want);
  const rows = fill(await o.ev(() => globalThis.__sb.index()));
  assert.equal(byName(rows, "Shadow button").role, "button");
  assert.equal(byName(rows, "Fancy clickable").role, "button");
  // nearby: text above the field within 200 px; a field with a name only falls back to it
  assert.equal(byName(rows, "Phone").nearby, undefined);
  assert.equal(byName(rows, "zip_code").nearby, undefined);
  const text = await o.ev(() => globalThis.__sb.text());
  const all = text.map((b) => b.spans.map((sp) => sp.text).join("")).join(" / ");
  assert.ok(!/City|Phone|Company|Nickname|Send me news/.test(all), all);
});

test("index() after a typed value and a toggled checkbox reflects live state", async () => {
  const o = await open("register.html");
  const rows = fill(await o.ev(() => globalThis.__sb.index()));
  await o.page.fill("#email", "ada@example.org");
  await o.page.fill('input[type="password"]', "not-a-real-secret");
  await o.page.check('input[name="agree"]');
  const after = fill(await o.ev(() => globalThis.__sb.index()));
  assert.deepEqual(byName(after, "Email").state.value, { kind: "text", text: "ada@example.org" });
  assert.deepEqual(byName(after, "Password").state.value, { kind: "filled" });
  assert.equal(byName(after, "I agree to the Terms of Service").state.checked, true);
  assert.ok(!JSON.stringify(after).includes("not-a-real-secret"));
  assert.equal(after.length, rows.length);
});

test("cost: a 2 000-row page walks and indexes within budget; index() after a small change is cheap", async (t) => {
  let body = "<main><h1>Catalogue</h1>";
  for (let i = 0; i < 400; i++) {
    body += `<div class="item"><h3>Item ${i}</h3><p>Description of item ${i}.</p>` +
      `<label>Qty <input name="q${i}"></label> <a href="/i/${i}">Open ${i}</a> <button>Add ${i}</button></div>`;
  }
  const big = await serve({ "/big.html": { body: "<!doctype html><html><body>" + body + "</main></body></html>" } });
  try {
    const o = await openSb(browser, big.origin + "/big.html");
    opened.push(o);
    const m = await o.ev(async () => {
      const sb = globalThis.__sb;
      const T = () => performance.now();
      const r = {};
      let t0 = T();
      r.count = sb.walk({ base: 1 }).count;
      r.walk = T() - t0;
      t0 = T();
      r.rows = sb.index().length;
      r.index = T() - t0;
      document.querySelectorAll(".item p")[200].textContent = "changed";
      await new Promise((res) => setTimeout(res, 0));
      t0 = T();
      const rows = sb.index();
      r.dirtyIndex = T() - t0;
      r.changedRow = rows.length;
      t0 = T();
      r.blocks = sb.text().length;
      r.text = T() - t0;
      return r;
    });
    t.diagnostic(JSON.stringify(Object.fromEntries(Object.entries(m).map(([k, v]) => [k, Math.round(v)]))));
    assert.equal(m.rows, 1601);
    assert.equal(m.changedRow, 1601);
    assert.ok(m.walk < 500, `walk ${m.walk} ms`);
    assert.ok(m.index < 100, `index ${m.index} ms`);
    assert.ok(m.dirtyIndex < 100, `dirty index ${m.dirtyIndex} ms`);
  } finally {
    await big.close();
  }
});
