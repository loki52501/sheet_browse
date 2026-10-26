// Layout-dependent parts of injected.js in real Chromium (D-401): visibility, occlusion,
// stability, act timings, settle cap, frames. injected.js is installed exactly as sb-driver
// installs it (Page.addScriptToEvaluateOnNewDocument, world "sb") and called in that world.
// docs/05 acceptance 1, the in-page parts of 2 and 5, and the index side of 6.
"use strict";

const { test, before, after } = require("node:test");
const assert = require("node:assert/strict");
const { serve, launch, openSb, byName, fill, page } = require("./helpers");

let browser;
let A; // origin 1
let B; // origin 2 (same site, other port: a real cross-origin frame)
const opened = [];

// Small in-memory pages for single conditions (no network, no fixed sleeps).
const html = (head, body) => ({ body: `<!doctype html><html><head><meta charset="utf-8">${head}</head><body>${body}</body></html>` });
const EXTRA = {
  "/posts/how-captchas-work": { body: page("how-captchas-work.html") },
  // a script that renders nothing: settle's content wait ends at content_ms
  "/scripted-empty.html": html("<title>Empty app</title><script>window.__app = 1;</script>", ""),
  "/pdf.html": html("<title>PDF</title><script>window.__viewer = 1;</script>",
    '<embed type="application/pdf" src="/no-such.pdf" width="600" height="400">'),
  "/anim.html": html("<title>Shimmer</title><style>.sh { animation: sh 800ms linear infinite; }" +
    " @keyframes sh { from { opacity: .4; } to { opacity: 1; } }</style>",
    '<p class="sh">Loading the feed</p><button type="button">Refresh</button>'),
  "/masked.html": html("<title>Masked</title><style>.pin { -webkit-text-security: disc; }</style>",
    '<form><input id="pin" class="pin" type="text" aria-label="PIN">' +
    '<input id="pw" type="password" aria-label="Password">' +
    '<button id="show" type="button" onclick="document.getElementById(\'pw\').type = \'text\'">Show</button></form>'),
  // a show-password toggle that rebuilds the element and hands it the value (D-429 (1))
  "/rebuild.html": html("<title>Rebuild</title><script>function rebuildPw() { const o = document.getElementById('pw');" +
    " const n = document.createElement('input'); n.type = 'text'; n.id = 'plain-copy'; n.name = 'shown';" +
    " n.setAttribute('aria-label', 'Password'); n.value = o.value; o.replaceWith(n); }</script>",
    '<form><input id="user" aria-label="Username"><span id="slot"><input id="pw" type="password" aria-label="Password"></span>' +
    '<button id="show" type="button" onclick="rebuildPw()">Show</button><button>Sign in</button></form>'),
  // challenge text false positives and the real prompt (D-429 (2))
  "/hold.html": html("<title>Hold</title>",
    '<main><p>Press and hold Shift to select several files.</p><input type="file" aria-label="Files">' +
    '<p>Press and hold the mic button to talk</p><button type="button">Mic</button>' +
    '<section><h2>Are you human?</h2><p>A short quiz.</p><button type="button">Start</button></section>' +
    '<div id="px"><p>Press &amp; Hold to confirm you are a human (and not a bot).</p><div id="px-captcha" style="width:300px;height:50px"></div></div></main>'),
  // D-435 (1): an SPA login that swaps #app; the app page's pre-filled fields match nothing
  "/spa-login.html": html(`<title>SPA login</title><script>function signIn() {
    const app = document.createElement("div"); app.id = "app";
    app.innerHTML = '<h1>Profile</h1><input id="dn" aria-label="Display name" value="Ada Lovelace">' +
      '<input name="city" placeholder="City" value="London"><label>Bio <textarea id="bio">Mathematician</textarea></label>';
    document.getElementById("app").replaceWith(app); }</script>`,
  `<div id="app"><form><input id="email" aria-label="Email"><input id="pw" type="password" name="password" aria-label="Password" placeholder="Password">
    <button id="go" type="button" onclick="signIn()">Sign in</button></form></div>`),
  // D-435 (1): a show toggle that replaceWith-swaps in an input sharing no identity
  "/swap-neutral.html": html(`<title>Swap</title><script>function swapPw() { const o = document.getElementById("pw");
    const n = document.createElement("input"); n.id = "plain"; n.name = "shown"; n.setAttribute("aria-label", "Visible text");
    n.value = o.value; o.replaceWith(n); }</script>`,
  `<form><input id="user" aria-label="User"><span><input id="pw" type="password" aria-label="Password"></span>
    <button id="swap" type="button" onclick="swapPw()">Show</button></form>`),
  // D-435 (2): same-identity rebuilds split by a microtask, a timer, or more than 2 s
  "/split.html": html(`<title>Split</title><script>
    function copyOf(o) { const n = document.createElement("input"); n.type = "text"; n.id = o.id + "-copy";
      n.setAttribute("aria-label", o.getAttribute("aria-label")); n.value = o.value; return n; }
    async function splitMicro() { const o = document.getElementById("k1"); const p = o.parentNode; const n = copyOf(o);
      o.remove(); await null; p.appendChild(n); }
    function splitTimer() { const o = document.getElementById("k2"); const p = o.parentNode; const n = copyOf(o);
      o.remove(); setTimeout(() => p.appendChild(n), 30); }
    function removeK3() { const o = document.getElementById("k3"); window.k3 = [copyOf(o), o.parentNode]; o.remove(); }
    function addK3() { window.k3[1].appendChild(window.k3[0]); }</script>`,
  `<form><div><input id="k1" type="password" aria-label="Key one"></div><div><input id="k2" type="password" aria-label="Key two"></div>
    <div><input id="k3" type="password" aria-label="Key three"></div></form>`),
};
// D-436: a quiz heading and hold text next to passive v3 markers, or next to a visible v2 checkbox
const QUIZ = '<section><h2>Are you human?</h2><p>Take our quiz about what makes us human.</p>' +
  '<p>Press and hold the button to start</p><button type="button">Start the quiz</button></section>';
EXTRA["/quiz-v3.html"] = html("<title>Quiz</title>", "<main>" + QUIZ +
  '<form><input aria-label="Email"><button class="g-recaptcha" data-sitekey="k" data-callback="cb" type="submit">Send</button></form></main>' +
  '<div class="grecaptcha-badge" style="position:fixed;right:0;bottom:14px;width:256px;height:60px"><div class="grecaptcha-logo">' +
  '<iframe title="reCAPTCHA" srcdoc="&lt;p&gt;badge&lt;/p&gt;" style="width:256px;height:60px"></iframe></div>' +
  '<textarea name="g-recaptcha-response" class="g-recaptcha-response" style="display:none"></textarea></div>' +
  '<div style="visibility:hidden;position:absolute;top:-9999px"><iframe title="recaptcha challenge expires in two minutes" srcdoc="&lt;p&gt;c&lt;/p&gt;"></iframe></div>');
EXTRA["/quiz-v2.html"] = html("<title>Quiz</title>", "<main>" + QUIZ +
  '<div class="g-recaptcha" data-sitekey="k" style="width:304px;height:78px"><iframe title="reCAPTCHA" width="304" height="78" ' +
  'srcdoc="&lt;p&gt;checkbox&lt;/p&gt;"></iframe></div></main>');

// D-442: /slow.js is a parser-blocking script the server holds until the test releases it
// (d442-loading.html stays "loading" for as long as the test needs, with no fixed delay).
let slowGate = null;
EXTRA["/slow.js"] = { body: "/* released by the test */", type: "text/javascript", hold: () => (slowGate ? slowGate.promise : null) };
function holdSlow() {
  let release;
  const promise = new Promise((r) => { release = r; });
  slowGate = { promise, release };
  return release;
}

// Read through every __sb reader in world sb while counting value getter calls on the given ids.
function readCounting(o, ids) {
  return o.ev(async (watch) => {
    const proto = HTMLInputElement.prototype;
    const desc = Object.getOwnPropertyDescriptor(proto, "value");
    const reads = {};
    Object.defineProperty(proto, "value", {
      configurable: true,
      get() {
        if (watch.includes(this.id)) reads[this.id] = (reads[this.id] || 0) + 1;
        return desc.get.call(this);
      },
      set: desc.set,
    });
    try {
      const sb = globalThis.__sb;
      sb.walk({ dirty: true });
      const index = sb.index();
      const out = [index, sb.text()];
      for (const id of watch) {
        const el = document.getElementById(id);
        if (el) out.push(sb.state(el), sb.name(el));
      }
      sb.walk({});
      out.push(sb.index());
      return { reads, blob: JSON.stringify(out), index };
    } finally {
      Object.defineProperty(proto, "value", desc);
    }
  }, ids);
}

before(async () => {
  [A, B] = await Promise.all([serve(EXTRA), serve(EXTRA)]);
  browser = await launch();
});
after(async () => {
  for (const o of opened) await o.close().catch(() => undefined);
  if (browser) await browser.close();
  if (A) await A.close();
  if (B) await B.close();
});

// Open a fixture and run the first based walk, as sb-driver does after each navigation
// (D-405 (2)). opts.base: the base (default 1); null skips the walk.
async function open(name, opts = {}) {
  const url = /^[a-z]+:/.test(name) ? name : A.origin + "/" + name;
  const o = await openSb(browser, url, opts);
  opened.push(o);
  if (opts.base !== null) await o.ev((b) => globalThis.__sb.walk({ base: b }), opts.base || 1);
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
  const o = await open("register.html", { base: null });
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

// settle() timed in the page.
const timedSettle = (o, opts) => o.ev(async (op) => {
  const t = performance.now();
  const r = await globalThis.__sb.settle(op || undefined);
  return { r, ms: performance.now() - t };
}, opts);

test("settle skips the content wait (D-405 (8)): about:blank, script-less empty body, PDF embed, infinite animation", async (t) => {
  // about:blank (a fresh or popup tab before navigation)
  const blank = await open("about:blank");
  const b = await timedSettle(blank);
  t.diagnostic(`settle about:blank: ${b.ms.toFixed(0)} ms`);
  assert.equal(b.r.settled, true);
  assert.ok(b.ms < 400, `about:blank settle took ${b.ms.toFixed(0)} ms`);
  // a loaded page with an empty body and no scripts
  const empty = await open("accept-blank.html");
  const e = await timedSettle(empty);
  t.diagnostic(`settle empty body, no scripts: ${e.ms.toFixed(0)} ms`);
  assert.equal(e.r.settled, true);
  assert.ok(e.ms < 400, `empty-body settle took ${e.ms.toFixed(0)} ms`);
  // a PDF embed is content even with a script on the page
  const pdf = await open("pdf.html");
  const p = await timedSettle(pdf);
  assert.equal(p.r.settled, true);
  assert.ok(p.ms < 400, `pdf settle took ${p.ms.toFixed(0)} ms`);
  // a scripted page that never renders: the content wait ends at content_ms, not the cap
  const sc = await open("scripted-empty.html");
  const s1 = await timedSettle(sc);
  t.diagnostic(`settle scripted empty body: ${s1.ms.toFixed(0)} ms`);
  assert.equal(s1.r.settled, true);
  assert.ok(s1.ms >= 990 && s1.ms < 1200, `scripted empty settle took ${s1.ms.toFixed(0)} ms`);
  const s2 = await timedSettle(sc, { content_ms: 300 });
  assert.ok(s2.ms >= 290 && s2.ms < 450, `content_ms 300 took ${s2.ms.toFixed(0)} ms`);
  // an infinite shimmer plus a class toggle on <html> does not hold settle to the cap
  const an = await open("anim.html");
  await an.page.evaluate(() => document.documentElement.classList.add("scrolled"));
  const a = await timedSettle(an);
  assert.equal(a.r.settled, true);
  assert.ok(a.ms < 500, `infinite animation settle took ${a.ms.toFixed(0)} ms`);
  assert.ok(await an.page.evaluate(() => document.getAnimations().some((x) => x.playState === "running")));
});

test("CHALLENGE in Chromium: interstitial with a closed shadow root, widgets, v3 badge not, bframe once visible", async () => {
  const it = await open("challenge-interstitial.html");
  const rows = fill(await it.ev(() => globalThis.__sb.index()));
  const ch = rows.filter((r) => r.flags.challenge);
  assert.equal(ch.length, 2, JSON.stringify(rows));
  assert.equal(ch[0].name, "Verify you are human by completing the action below.");
  assert.ok(byName(rows, "Why am I seeing this page?", "link"));
  assert.deepEqual((await timedAct(it, ch[1].id)).r, { why: "challenge" });

  const w = await open("challenge-widgets.html");
  const ids = (sel) => w.ev((s) => globalThis.__sb.id(document.querySelector(s)), sel);
  const wr = fill(await w.ev(() => globalThis.__sb.index()));
  const want = [await ids("#arkose iframe"), await ids(".geetest_holder"), await ids("#hc")].sort((x, y) => x - y);
  assert.deepEqual(wr.filter((r) => r.flags.challenge).map((r) => r.id).sort((x, y) => x - y), want);
  const bframeId = await ids("#bframe-box iframe");
  const bf = wr.find((r) => r.id === bframeId);
  assert.ok(bf && !bf.flags.challenge && bf.state.visible === false, JSON.stringify(bf));
  const badgeId = await ids(".grecaptcha-badge iframe");
  const badge = wr.find((r) => r.id === badgeId);
  assert.ok(badge && !badge.flags.challenge, JSON.stringify(badge));
  assert.ok(!byName(wr, "Sign in", "button").flags.challenge);
  // the site shows the challenge: the bframe becomes visible and is CHALLENGE
  await w.page.evaluate(() => {
    const b = document.getElementById("bframe-box");
    b.style.visibility = "visible";
    b.style.top = "10px";
  });
  await w.ev(() => globalThis.__sb.walk({ dirty: true }));
  const shown = fill(await w.ev(() => globalThis.__sb.index()));
  const bf2 = shown.find((r) => r.id === bframeId);
  assert.ok(bf2.flags.challenge && bf2.state.visible, JSON.stringify(bf2));

  // a URL that only mentions captcha is an ordinary article
  const art = await open("posts/how-captchas-work");
  const ar = fill(await art.ev(() => globalThis.__sb.index()));
  assert.ok(!ar.some((r) => r.flags.challenge));
  assert.ok(byName(ar, "How captchas work", "heading") && byName(ar, "Subscribe", "button"));
  assert.ok((await art.ev(() => globalThis.__sb.text())).length > 3);
});

test("masked and toggled password fields stay empty|filled in Chromium (D-405 (1))", async () => {
  const o = await open("masked.html");
  await o.page.fill("#pin", "masked-canary-1");
  await o.page.fill("#pw", "toggled-canary-2");
  const before = fill(await o.ev(() => globalThis.__sb.index()));
  assert.deepEqual(byName(before, "PIN").state.value, { kind: "filled" });
  assert.deepEqual(byName(before, "Password").state.value, { kind: "filled" });
  await o.page.click("#show"); // the site's own show-password toggle
  assert.equal(await o.page.evaluate(() => document.getElementById("pw").type), "text");
  const after = fill(await o.ev(() => globalThis.__sb.index()));
  assert.deepEqual(byName(after, "Password").state.value, { kind: "filled" });
  const blob = JSON.stringify(after) + JSON.stringify(await o.ev(() => globalThis.__sb.text())) +
    JSON.stringify(await o.ev(() => globalThis.__sb.state(document.getElementById("pw"))));
  assert.ok(!blob.includes("canary"), blob);
});

test("a password field rebuilt by a show toggle stays empty|filled in Chromium (D-429 (1))", async () => {
  const o = await open("rebuild.html");
  await o.page.fill("#user", "ada");
  await o.page.fill("#pw", "rebuilt-canary-3");
  const before = fill(await o.ev(() => globalThis.__sb.index()));
  assert.deepEqual(byName(before, "Password").state.value, { kind: "filled" });
  await o.page.click("#show"); // replaceWith a type=text input that carries the value
  assert.equal(await o.page.evaluate(() => document.getElementById("plain-copy").value), "rebuilt-canary-3");
  // count value getter calls on the rebuilt field inside world sb while it reads
  const r = await o.ev(() => {
    const proto = HTMLInputElement.prototype;
    const desc = Object.getOwnPropertyDescriptor(proto, "value");
    let reads = 0;
    Object.defineProperty(proto, "value", {
      configurable: true,
      get() {
        if (this.id === "plain-copy") reads++;
        return desc.get.call(this);
      },
      set: desc.set,
    });
    try {
      globalThis.__sb.walk({ dirty: true });
      const index = globalThis.__sb.index();
      const text = globalThis.__sb.text();
      const state = globalThis.__sb.state(document.getElementById("plain-copy"));
      globalThis.__sb.walk({});
      const full = globalThis.__sb.index();
      return { reads, blob: JSON.stringify([index, text, state, full]), index };
    } finally {
      Object.defineProperty(proto, "value", desc);
    }
  });
  assert.equal(r.reads, 0, "the value getter of the rebuilt field was called");
  assert.ok(!r.blob.includes("canary"), r.blob);
  assert.deepEqual(byName(r.index, "Password").state.value, { kind: "filled" });
  assert.deepEqual(byName(r.index, "Username").state.value, { kind: "text", text: "ada" });
});

test("challenge text in Chromium (D-429 (2)): press-and-hold UI and a quiz heading are not CHALLENGE, PerimeterX is", async () => {
  const o = await open("hold.html");
  const rows = fill(await o.ev(() => globalThis.__sb.index()));
  const ch = rows.filter((r) => r.flags.challenge);
  const px = await o.ev(() => globalThis.__sb.id(document.querySelector("#px p")));
  const box = await o.ev(() => globalThis.__sb.id(document.getElementById("px-captcha")));
  assert.ok(ch.some((r) => r.id === px) && ch.some((r) => r.id === box), JSON.stringify(ch));
  // with the #px-captcha widget on the page "Are you human?" and press-and-hold text count as
  // prompts (D-429 (2), D-435 (3)), while the controls next to them stay ordinary rows; once
  // the widget is gone nothing on the page is CHALLENGE
  assert.ok(byName(rows, "Are you human?", "heading").flags.challenge);
  assert.ok(byName(rows, "Press and hold Shift to select several files.").flags.challenge);
  assert.ok(!ch.some((r) => /^(Files|Mic|Start)$/.test(r.name)), JSON.stringify(ch));
  await o.page.evaluate(() => document.getElementById("px").remove());
  await o.ev(() => globalThis.__sb.walk({ dirty: true }));
  const after = fill(await o.ev(() => globalThis.__sb.index()));
  assert.ok(!after.some((r) => r.flags.challenge), JSON.stringify(after.filter((r) => r.flags.challenge)));
  assert.ok(byName(after, "Are you human?", "heading"));
  const text = (await o.ev(() => globalThis.__sb.text())).map((b) => b.spans.map((x) => x.text).join("")).join(" / ");
  assert.match(text, /Press and hold Shift/);
});

test("an SPA login swap in Chromium: the app page's pre-filled fields read their values (D-435 (1))", async () => {
  const o = await open("spa-login.html");
  await o.page.fill("#email", "ada@example.org");
  await o.page.fill("#pw", "spa-canary-435");
  assert.deepEqual(byName(fill(await o.ev(() => globalThis.__sb.index())), "Password").state.value, { kind: "filled" });
  await o.page.click("#go");
  const r = await readCounting(o, ["pw", "dn"]);
  assert.ok(!r.blob.includes("spa-canary"), r.blob);
  assert.equal(r.reads.pw, undefined);
  const rows = fill(r.index);
  assert.deepEqual(byName(rows, "Display name").state.value, { kind: "text", text: "Ada Lovelace" });
  assert.deepEqual(byName(rows, "City").state.value, { kind: "text", text: "London" });
  assert.deepEqual(byName(rows, "Bio").state.value, { kind: "text", text: "Mathematician" });
});

test("a show toggle that replaceWith-swaps in an unrelated input never reads the value (D-435 (1))", async () => {
  const o = await open("swap-neutral.html");
  await o.page.fill("#user", "ada");
  await o.page.fill("#pw", "swap-canary-435");
  await o.ev(() => globalThis.__sb.index());
  await o.page.click("#swap");
  assert.equal(await o.page.evaluate(() => document.getElementById("plain").value), "swap-canary-435");
  const r = await readCounting(o, ["plain"]);
  assert.deepEqual(r.reads, {}, "the value getter of the swapped-in field was called");
  assert.ok(!r.blob.includes("canary"), r.blob);
  const rows = fill(r.index);
  assert.deepEqual(byName(rows, "Visible text").state.value, { kind: "filled" });
  assert.deepEqual(byName(rows, "User").state.value, { kind: "text", text: "ada" });
});

test("same-identity rebuilds split by a microtask or a timer stay sensitive; after 2 s they read (D-435 (2))", async () => {
  const o = await open("split.html");
  // a controllable clock for injected.js: world sb's own performance.now
  await o.ev(() => {
    const real = performance.now.bind(performance);
    let off = 0;
    Object.defineProperty(performance, "now", { configurable: true, value: () => real() + off });
    globalThis.__advance = (ms) => { off += ms; };
  });
  await o.page.fill("#k1", "split-canary-1");
  await o.page.fill("#k2", "split-canary-2");
  await o.page.fill("#k3", "not a secret");
  await o.ev(() => globalThis.__sb.index());
  await o.page.evaluate(() => globalThis.splitMicro());
  await o.page.evaluate(() => globalThis.splitTimer());
  await o.page.waitForFunction(() => !!document.getElementById("k2-copy"));
  await o.page.evaluate(() => globalThis.removeK3());
  await o.ev(() => globalThis.__advance(2100));
  await o.page.evaluate(() => globalThis.addK3());
  const r = await readCounting(o, ["k1-copy", "k2-copy"]);
  assert.deepEqual(r.reads, {}, "a rebuilt field's value getter was called");
  assert.ok(!r.blob.includes("split-canary"), r.blob);
  const rows = fill(r.index);
  assert.deepEqual(byName(rows, "Key one").state.value, { kind: "filled" });
  assert.deepEqual(byName(rows, "Key two").state.value, { kind: "filled" });
  assert.deepEqual(byName(rows, "Key three").state.value, { kind: "text", text: "not a secret" });
});

test("an add-first show toggle that removes the old field on transitionend never reads the value (D-438 (1))", async () => {
  const o = await open("fade-show.html");
  await o.page.fill("#acct", "fade-canary-1");
  await o.page.fill("#k2", "fade-canary-2");
  await o.page.fill("#user", "ada");
  await o.ev(() => globalThis.__sb.index());
  const gone = (id) => o.page.waitForFunction((i) => !document.getElementById(i), id);
  // 1. the copy keeps name and aria-label: sensitive while both fields exist (the overlap read)
  await o.page.click("#show1");
  assert.equal(await o.page.evaluate(() => document.getElementById("acct-t").value), "fade-canary-1");
  const mid = await readCounting(o, ["acct-t"]);
  assert.ok(await o.page.evaluate(() => !!document.getElementById("acct")), "the old field is still there");
  assert.deepEqual(mid.reads, {}, "read during the overlap");
  assert.ok(!mid.blob.includes("fade-canary"), mid.blob);
  // and after the old field leaves on transitionend
  await o.page.evaluate(() => globalThis.fade("acct"));
  await gone("acct");
  // 2. the copy shares nothing: the single field added to the old field's parent
  await o.page.click("#show2");
  await o.page.evaluate(() => globalThis.fade("k2"));
  await gone("k2");
  const r = await readCounting(o, ["acct-t", "k2-plain"]);
  assert.deepEqual(r.reads, {}, "a rebuilt field's value getter was called");
  assert.ok(!r.blob.includes("fade-canary"), r.blob);
  const rows = fill(r.index);
  assert.deepEqual(byName(rows, "Account key").state.value, { kind: "filled" });
  assert.deepEqual(byName(rows, "Visible key").state.value, { kind: "filled" });
  assert.deepEqual(byName(rows, "User").state.value, { kind: "text", text: "ada" });
});

test("an add-first show toggle whose leave swaps in a different field never reads the value (D-441)", async () => {
  const o = await open("d441.html");
  await o.page.fill("#acct", "d441-canary");
  await o.page.fill("#user", "ada");
  await o.ev(() => globalThis.__sb.index());
  // the neighbour is added after load (not a parser insert), with no key shared with #acct
  assert.equal(await o.page.evaluate(() => document.readyState), "complete");
  await o.page.click("#toggle");
  assert.equal(await o.page.evaluate(() => document.getElementById("q9").value), "d441-canary");
  // the leave: on transitionend #acct is replaceWith'd by #hint (a D-437 swap in that batch)
  await o.page.evaluate(() => globalThis.fade());
  await o.page.waitForFunction(() => !document.getElementById("acct") && !!document.getElementById("hint"));
  const r = await readCounting(o, ["q9", "hint"]);
  assert.deepEqual(r.reads, {}, "a value getter was called after the removal");
  assert.ok(!r.blob.includes("d441-canary"), r.blob);
  // out: [index, text, state(q9), name(q9), state(hint), name(hint), index]
  const out = JSON.parse(r.blob);
  assert.deepEqual(out[2].value, { kind: "filled" }, "q9");
  assert.deepEqual(out[4].value, { kind: "empty" }, "hint");
  assert.deepEqual(byName(fill(r.index), "User").state.value, { kind: "text", text: "ada" });
});

// D-442 (review-T-19 round 6 M1, M4): a copy added while a sync script keeps the page "loading"
// is not a parser insert; it inherits like any other remembered field of the parent.
async function openLoading() {
  const o = await open("d442-loading.html", { waitUntil: "commit", base: null });
  await o.page.waitForSelector("#toggle");
  await o.ev(() => globalThis.__sb.walk({ base: 1 }));
  await o.page.fill("#acct", "d442-canary");
  await o.page.fill("#user", "ada");
  await o.ev(() => globalThis.__sb.index());
  assert.equal(await o.page.evaluate(() => document.readyState), "loading", "the held script keeps the page loading");
  await o.page.click("#toggle", { noWaitAfter: true });
  assert.equal(await o.page.evaluate(() => document.getElementById("q9").value), "d442-canary");
  assert.equal(await o.page.evaluate(() => document.readyState), "loading", "the copy was added while loading");
  return o;
}
function checkLeak(r, label) {
  assert.deepEqual(r.reads, {}, label + ": a value getter was called after the removal");
  assert.ok(!r.blob.includes("d442-canary"), label + ": " + r.blob);
  // out: [index, text, state(q9), name(q9), state(hint), name(hint), index]
  const out = JSON.parse(r.blob);
  assert.deepEqual(out[2].value, { kind: "filled" }, label + ": q9");
  assert.deepEqual(out[4].value, { kind: "empty" }, label + ": hint");
  assert.deepEqual(byName(fill(r.index), "User").state.value, { kind: "text", text: "ada" }, label + ": User");
}

test("an add-first show toggle whose leave swaps in a different field while the page is loading never reads the value, then or after load (D-442)", async () => {
  const release = holdSlow();
  try {
    const o = await openLoading();
    await o.page.evaluate(() => globalThis.fade());
    await o.page.waitForFunction(() => !document.getElementById("acct") && !!document.getElementById("hint"));
    assert.equal(await o.page.evaluate(() => document.readyState), "loading", "the swap landed while loading");
    checkLeak(await readCounting(o, ["q9", "hint"]), "while loading");
    release();
    await o.page.waitForFunction(() => document.readyState === "complete");
    checkLeak(await readCounting(o, ["q9", "hint"]), "after load");
  } finally {
    release();
  }
});

test("a copy added while the page is loading, then a swap after load within 2 s, never reads the value (D-442)", async () => {
  const release = holdSlow();
  try {
    const o = await openLoading();
    const t0 = await o.page.evaluate(() => performance.now());
    release();
    await o.page.waitForFunction(() => document.readyState !== "loading");
    const dt = await o.page.evaluate(() => {
      globalThis.swap();
      return performance.now();
    }) - t0;
    assert.ok(dt < 2000, "the swap landed " + Math.round(dt) + " ms after the copy, inside the 2 s window");
    checkLeak(await readCounting(o, ["q9", "hint"]), "after load");
  } finally {
    release();
  }
});

test("a remove-first show toggle that appends an unrelated field in a later task never reads the value (D-439)", async () => {
  const o = await open("remove-first.html");
  await o.page.fill("#key", "rf-canary-439");
  await o.page.fill("#user", "ada");
  await o.ev(() => globalThis.__sb.index());
  await o.page.click("#show");
  await o.page.waitForFunction(() => !!document.getElementById("shown"));
  assert.equal(await o.page.evaluate(() => document.getElementById("shown").value), "rf-canary-439");
  const r = await readCounting(o, ["shown"]);
  assert.deepEqual(r.reads, {}, "the new field's value getter was called");
  assert.ok(!r.blob.includes("rf-canary"), r.blob);
  const rows = fill(r.index);
  assert.deepEqual(byName(rows, "Visible text").state.value, { kind: "filled" });
  assert.deepEqual(byName(rows, "User").state.value, { kind: "text", text: "ada" });
});

test("passive reCAPTCHA v3 markers are not challenge context in Chromium; a visible v2 checkbox is (D-436, D-435 (3))", async () => {
  const v3 = await open("quiz-v3.html");
  const r3 = fill(await v3.ev(() => globalThis.__sb.index()));
  assert.deepEqual(r3.filter((r) => r.flags.challenge).map((r) => r.role + "|" + r.name), []);
  assert.ok(byName(r3, "Are you human?", "heading"));
  const t3 = (await v3.ev(() => globalThis.__sb.text())).map((b) => b.spans.map((x) => x.text).join("")).join(" / ");
  assert.match(t3, /Press and hold the button to start/);
  const v2 = await open("quiz-v2.html");
  const r2 = fill(await v2.ev(() => globalThis.__sb.index()));
  assert.ok(byName(r2, "Are you human?", "heading").flags.challenge, JSON.stringify(r2));
  assert.ok(byName(r2, "Press and hold the button to start").flags.challenge);
  assert.ok(!byName(r2, "Start the quiz").flags.challenge);
});

test("register fixtures (D-405 (10)) match the live index() and text()", async () => {
  const fs = require("node:fs");
  const path = require("node:path");
  const dir = path.join(__dirname, "fixtures");
  const o = await open("register.html");
  const live = await o.ev(() => ({ index: globalThis.__sb.index(), text: globalThis.__sb.text() }));
  const idx = JSON.parse(fs.readFileSync(path.join(dir, "register.index.json"), "utf8"));
  const txt = JSON.parse(fs.readFileSync(path.join(dir, "register.text.json"), "utf8"));
  // boxes depend on the machine's fonts: compare everything else exactly
  const noBox = (rows) => rows.map((r) => Object.assign({}, r, { box: r.box ? "box" : undefined }));
  assert.deepEqual(noBox(live.index), noBox(idx), "regenerate with node injected/test/gen-fixtures.js");
  assert.deepEqual(live.text, txt, "regenerate with node injected/test/gen-fixtures.js");
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
  const capF = tree.find((f) => f.url.includes("/cdn-cgi/challenge-platform/"));
  assert.ok(sameF && crossF && capF, JSON.stringify(tree));
  // sb-driver walks each frame with its own base before reading it (D-405 (2))
  const walkIndex = (fid) => o.ev(() => {
    globalThis.__sb.walk({ base: 1 });
    return globalThis.__sb.index();
  }, undefined, fid);
  const childRows = fill(await walkIndex(sameF.id));
  assert.ok(byName(childRows, "Nickname", "textbox"));
  assert.equal(byName(childRows, "Save nickname").form, "f1");
  const crossRows = fill(await walkIndex(crossF.id));
  assert.ok(byName(crossRows, "Nickname"));
  // a challenge frame (by its path, D-405 (3)) is one CHALLENGE row; nothing inside is read (D-5)
  const capRows = fill(await walkIndex(capF.id));
  assert.equal(capRows.length, 1);
  assert.equal(capRows[0].flags.challenge, true);
  assert.ok(!byName(capRows, "Nickname"));
  const capText = await o.ev(() => globalThis.__sb.text(), undefined, capF.id);
  assert.equal(capText.length, 1);
  assert.ok(!JSON.stringify(capText).includes("Nickname"));
  assert.equal((await o.ev(() => globalThis.__sb.frames(), undefined, capF.id))[0].challenge, true);
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

// D-443 (1) (review-T-19 round 7 O1..O3, runs/reviewer-12/probe-r12-chromium.js): a neutral
// add-first copy is never read while the password is still in the page, also after settle()
// resolved first. The world-sb counter on the password and the copy stays installed for every
// read: one right after the click, one in the same evaluate right after settle() (nothing
// awaited in between), and one once the old field is gone.
function overlapRead(o, n, settle) {
  return o.ev(async (a) => {
    const sb = globalThis.__sb;
    const s = a.settle ? await sb.settle() : null;
    const pw = !!document.getElementById("acct" + a.n);
    sb.walk({ dirty: true });
    const out = [sb.index(), sb.text(), sb.fingerprint('textbox "User" textbox "Account key one"')];
    for (const el of document.querySelectorAll("input")) out.push(sb.state(el), sb.name(el));
    const copy = document.getElementById("q" + a.n);
    const r = { settled: s ? s.settled : null, pw, copy: copy ? sb.state(copy).value : null };
    r.user = sb.state(document.getElementById("user")).value;
    sb.walk({});
    out.push(sb.index(), sb.text());
    r.blob = JSON.stringify(out);
    return r;
  }, { n, settle });
}
async function overlapCase(t, n, label) {
  const CAN = "d443-canary-o" + n;
  const o = await open("d443-overlap.html");
  await o.page.fill("#acct" + n, CAN);
  await o.page.fill("#user", "ada");
  await o.ev(() => globalThis.__sb.index());
  await o.ev((ids) => {
    const proto = HTMLInputElement.prototype;
    const desc = Object.getOwnPropertyDescriptor(proto, "value");
    globalThis.__d443 = {};
    Object.defineProperty(proto, "value", {
      configurable: true,
      get() {
        if (ids.includes(this.id)) globalThis.__d443[this.id] = (globalThis.__d443[this.id] || 0) + 1;
        return desc.get.call(this);
      },
      set: desc.set,
    });
  }, ["acct" + n, "q" + n]);
  await o.page.click("#b" + n, { noWaitAfter: true });
  const mid = await overlapRead(o, n, false);
  assert.ok(mid.pw, label + ": the first read ran while the password was in the page");
  const same = await overlapRead(o, n, true);
  await o.page.waitForFunction((id) => !document.getElementById(id), "acct" + n);
  const after = await overlapRead(o, n, false);
  const reads = await o.ev(() => globalThis.__d443);
  t.diagnostic(`${label}: settle() settled ${same.settled}, password in the page at the same-evaluate read ${same.pw}; ` +
    `copy kinds mid ${mid.copy && mid.copy.kind}, same ${same.copy && same.copy.kind}, after ${after.copy && after.copy.kind}; reads ${JSON.stringify(reads)}`);
  assert.deepEqual(reads, {}, label + ": a value getter was called");
  for (const [k, r] of Object.entries({ mid, same, after })) {
    assert.ok(!r.blob.includes(CAN), label + " " + k + ": the value is in __sb output");
    assert.ok(r.copy && (r.copy.kind === "empty" || r.copy.kind === "filled"), label + " " + k + ": copy " + (r.copy && r.copy.kind));
    assert.deepEqual(r.user, { kind: "text", text: "ada" }, label + " " + k + ": User");
  }
  // gone: the parent lost its only value field, a sensitive one, so the copy inherits (D-442)
  assert.deepEqual(after.copy, { kind: "filled" }, label + ": copy after the removal");
}

test("O1 D-443: a CSS-faded add-first show toggle, read in the same evaluate right after settle(), never reads the value", async (t) => {
  await overlapCase(t, 1, "O1 CSS fade");
});

test("O2 D-443: a JS-timed exit with no animation never reads the value, mid-exit or after", async (t) => {
  await overlapCase(t, 2, "O2 timer exit");
});

test("O3 D-443: an exit class on the wrapper with the transition on the inner input never reads the value", async (t) => {
  await overlapCase(t, 3, "O3 child transition");
});

// D-444 (1) (review-T-19 round 8 Y6, Y7, runs/reviewer-13/probe-r13-chromium.js): show toggles
// that swap visibility instead of adding and removing fields. Click Show, settle(), read in the
// same evaluate, read again, and read once the page clock is past the click + 2.1 s (past the
// D-443 overlap mask). The world-sb getter counter on the password and the plain field stays
// installed for every read.
function twinRead(o, n, settle) {
  return o.ev(async (a) => {
    const sb = globalThis.__sb;
    const s = a.settle ? await sb.settle() : null;
    sb.walk({ dirty: true });
    const out = [sb.index(), sb.text(), sb.fingerprint('textbox "User" textbox "Email" textbox "City"')];
    for (const el of document.querySelectorAll("input")) out.push(sb.state(el), sb.name(el));
    const st = (id) => sb.state(document.getElementById(id)).value;
    const r = { settled: s ? s.settled : null, pw: !!document.getElementById("acct" + a.n), copy: null };
    if (document.getElementById("q" + a.n)) r.copy = st("q" + a.n);
    r.user = st("user");
    r.email = st("email");
    r.city = st("city");
    sb.walk({});
    out.push(sb.index(), sb.text());
    r.blob = JSON.stringify(out);
    return r;
  }, { n, settle });
}
async function twinCase(t, n, label, page) {
  const CAN = "d444-canary-y" + n;
  const o = await open(page || "d444-twins.html");
  await o.page.fill("#acct" + n, CAN);
  await o.page.fill("#user", "ada");
  await o.page.fill("#email", "ada@example.com");
  await o.page.fill("#city", "London");
  await o.ev(() => globalThis.__sb.index());
  await o.ev((ids) => {
    const proto = HTMLInputElement.prototype;
    const desc = Object.getOwnPropertyDescriptor(proto, "value");
    globalThis.__d444 = {};
    Object.defineProperty(proto, "value", {
      configurable: true,
      get() {
        if (ids.includes(this.id)) globalThis.__d444[this.id] = (globalThis.__d444[this.id] || 0) + 1;
        return desc.get.call(this);
      },
      set: desc.set,
    });
  }, ["acct" + n, "q" + n]);
  const t0 = await o.ev(() => performance.now());
  await o.page.click("#b" + n, { noWaitAfter: true });
  const same = await twinRead(o, n, true);
  const next = await twinRead(o, n, false);
  await o.page.waitForFunction((at) => performance.now() > at + 2100, t0, { polling: 50 });
  const late = await twinRead(o, n, false);
  const reads = await o.ev(() => globalThis.__d444);
  t.diagnostic(`${label}: settled ${same.settled}; copy kinds same ${same.copy && same.copy.kind}, ` +
    `next ${next.copy && next.copy.kind}, late ${late.copy && late.copy.kind}; reads ${JSON.stringify(reads)}`);
  assert.equal(same.settled, true, label + ": settle() settled");
  assert.deepEqual(reads, {}, label + ": a value getter was called");
  for (const [k, r] of Object.entries({ same, next, late })) {
    assert.ok(r.pw, label + " " + k + ": the password stays in the page");
    assert.ok(!r.blob.includes(CAN), label + " " + k + ": the value is in __sb output");
    assert.ok(r.copy && (r.copy.kind === "empty" || r.copy.kind === "filled"), label + " " + k + ": copy " + JSON.stringify(r.copy));
    assert.deepEqual(r.user, { kind: "text", text: "ada" }, label + " " + k + ": User");
    assert.deepEqual(r.email, { kind: "text", text: "ada@example.com" }, label + " " + k + ": Email (control)");
    assert.deepEqual(r.city, { kind: "text", text: "London" }, label + " " + k + ": City (control)");
  }
}

test("Y6 D-444: clone and hide (copy added, password display:none and kept) never reads the value, also past 2 s", async (t) => {
  await twinCase(t, 6, "Y6 clone and hide");
});

test("Y7 D-444: a synced twin present from load, display flipped by Show, never reads the value", async (t) => {
  await twinCase(t, 7, "Y7 twin");
});

test("Y7w D-444: twin and password alone in sibling wrappers, the wrappers' display flipped, never reads the value", async (t) => {
  await twinCase(t, 8, "Y7w sibling wrappers");
});

test("Y7i D-444: twin and password alone in sibling wrappers, the inputs' display flipped, never reads the value", async (t) => {
  await twinCase(t, 9, "Y7i sibling wrappers, inputs flipped");
});

test("D-444 control: an ordinary visible form with no hidden sensitive field reads its text fields", async () => {
  const o = await open("d444-twins.html");
  await o.page.fill("#email", "ada@example.com");
  await o.page.fill("#city", "London");
  await o.page.fill("#pwc", "d444-control-canary");
  await o.page.fill("#user", "ada");
  const r = await o.ev(() => {
    const sb = globalThis.__sb;
    sb.walk({ dirty: true });
    const rows = sb.index();
    const v = (id) => sb.state(document.getElementById(id)).value;
    return { blob: JSON.stringify([rows, sb.text()]), email: v("email"), city: v("city"), user: v("user"), pw: v("pwc") };
  });
  assert.ok(!r.blob.includes("d444-control-canary"));
  assert.deepEqual(r.email, { kind: "text", text: "ada@example.com" }, "Email in a wrapper next to a visible password's wrapper");
  assert.deepEqual(r.city, { kind: "text", text: "London" }, "City next to a hidden field that is not sensitive");
  assert.deepEqual(r.user, { kind: "text", text: "ada" });
  assert.ok(r.pw.kind === "empty" || r.pw.kind === "filled");
});

// D-445 (review-T-19 round 9 Z3..Z7, runs/reviewer-14/probes/probe-r14-chromium.js): the same
// click, settle(), same/next/late protocol on bench/pages/d445-twins.html. (1) twin and password
// below wrappers: Z7 `x-in > div > input` with [hidden] on the hosts, Z7v a Vue v-show component
// (`div.base > label + div.control > input`) flipped on the base, Z7i the same shape with the
// inputs flipped, Z5m mixed depth inside one label. (2) the password hidden by opacity 0 (Z3),
// off-screen (Z4), sr-only (Z4v), clip-path inset(50%) (Z4c), clip rect 0 (Z4r), a 0x0 box (Z4z)
// or a clip-path url() (Z4u, a shape that is not parsed counts as hiding). (3) Z6: a Vue
// <Transition> whose transitionend handler sets display:none, read in the same evaluate as
// settle(); Z6s slides instead of fading, so only the transitionend handler hides the password.
for (const [n, label] of [
  [71, "Z7 hidden hosts, depth 2"],
  [72, "Z7v Vue v-show component, base flipped"],
  [73, "Z7i component shape, inputs flipped"],
  [51, "Z5m mixed depth inside one label"],
  [31, "Z3 opacity 0 password"],
  [41, "Z4 off-screen password"],
  [42, "Z4v sr-only password"],
  [43, "Z4c clip-path inset(50%) password"],
  [44, "Z4r clip rect(0 0 0 0) password"],
  [45, "Z4z zero-size password"],
  [46, "Z4u clip-path url() to an empty clipPath on the password"],
  [61, "Z6 Vue Transition + v-show, read chained after settle()"],
  [62, "Z6s slide Transition + v-show (no opacity), read chained after settle()"],
]) {
  test(`${label.split(" ")[0]} D-445: ${label.slice(label.indexOf(" ") + 1)} never reads the value, also past 2 s`, async (t) => {
    await twinCase(t, n, label, "d445-twins.html");
  });
}

test("D-445 control: an ordinary multi-div form with a visible password reads its other fields as text", async () => {
  const o = await open("d445-twins.html");
  await o.page.fill("#email", "ada@example.com");
  await o.page.fill("#city", "London");
  await o.page.fill("#pwc", "d445-control-canary");
  await o.page.fill("#user", "ada");
  const r = await o.ev(() => {
    const sb = globalThis.__sb;
    sb.walk({ dirty: true });
    const rows = sb.index();
    const v = (id) => sb.state(document.getElementById(id)).value;
    return { blob: JSON.stringify([rows, sb.text()]), email: v("email"), city: v("city"), user: v("user"), pw: v("pwc"), mail2: v("mail2") };
  });
  assert.ok(!r.blob.includes("d445-control-canary"));
  assert.deepEqual(r.mail2, { kind: "text", text: "pane@example.com" }, "beside a password below the fold of a scroll pane");
  assert.deepEqual(r.email, { kind: "text", text: "ada@example.com" }, "Email in a component beside a visible password's component");
  assert.deepEqual(r.city, { kind: "text", text: "London" }, "City beside a visible password's component and a hidden plain one");
  assert.deepEqual(r.user, { kind: "text", text: "ada" });
  assert.ok(r.pw.kind === "empty" || r.pw.kind === "filled");
});
