// media.js (B-9 in-page half) in real Chromium: video.list facts and seek + canvas frames.
// The clip is recorded by Playwright at test time (red, blue, green, white segments), so no
// binary fixture is committed. Captcha videos are never listed (D-5).
"use strict";

const { test, before, after } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { serve, launch, openSb } = require("./helpers");

let browser;
let A;
let B;
let clip;
const opened = [];

async function recordClip(br) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "sb-media-"));
  const ctx = await br.newContext({ viewport: { width: 320, height: 240 }, recordVideo: { dir, size: { width: 320, height: 240 } } });
  const pg = await ctx.newPage();
  // The page paints four 600 ms colour segments by its own clock; the test waits on its flag.
  await pg.setContent(`<body style="margin:0;background:#f00"><script>
    const cs = ["#f00", "#00f", "#0f0", "#fff"]; const t0 = performance.now();
    (function step() {
      const i = Math.floor((performance.now() - t0) / 600);
      if (i >= cs.length) { document.body.dataset.done = "1"; return; }
      document.body.style.background = cs[i]; requestAnimationFrame(step);
    })();
  </script></body>`);
  await pg.waitForFunction(() => document.body.dataset.done === "1", null, { timeout: 10000 });
  const v = pg.video();
  await ctx.close();
  return fs.readFileSync(await v.path());
}

const PAGE = (other) => `<!doctype html><html><body>
  <video id="v" src="/clip.webm" muted width="320" height="240"></video>
  <video id="x" src="${other}/clip.webm" muted width="160" height="120"></video>
  <div class="g-recaptcha"><video id="cap" src="/clip.webm" muted></video></div>
</body></html>`;

before(async () => {
  browser = await launch();
  clip = await recordClip(browser);
  const extra = { "/clip.webm": { type: "video/webm", body: clip } };
  [A, B] = await Promise.all([serve(extra), serve(extra)]);
  extra["/media.html"] = { body: PAGE(B.origin) };
});
after(async () => {
  for (const o of opened) await o.close().catch(() => undefined);
  if (browser) await browser.close();
  if (A) await A.close();
  if (B) await B.close();
});

async function openMedia() {
  const o = await openSb(browser, A.origin + "/media.html", { media: true });
  opened.push(o);
  await o.page.waitForFunction(() => ["v", "x"].every((i) => document.getElementById(i).readyState >= 2), null, { timeout: 10000 });
  return o;
}

test("video.list facts: kind, duration, src, visible, ids shared with __sb; captcha video excluded", async () => {
  const o = await openMedia();
  assert.equal(await o.page.evaluate(() => typeof window.__sbMedia), "undefined");
  const list = await o.ev(() => globalThis.__sbMedia.list());
  assert.equal(list.length, 2, JSON.stringify(list));
  const v = list[0];
  assert.equal(v.kind, "file");
  assert.ok(v.src.endsWith("/clip.webm"));
  assert.ok(v.duration > 2 && v.duration < 10, String(v.duration)); // recording length varies with load
  assert.equal(v.visible, true);
  assert.equal(v.paused, true);
  assert.equal(v.width, 320);
  assert.equal(v.id, await o.ev(() => globalThis.__sb.id(document.getElementById("v"))));
});

test("seek + canvas frames: one hash per scene, dedupe by hash, state restored, size cap", async () => {
  const o = await openMedia();
  const id = await o.ev(() => globalThis.__sb.id(document.getElementById("v")));
  const dur = await o.ev(() => document.getElementById("v").duration);
  const times = [];
  for (let t = 0.05; t < dur; t += 0.1) times.push(Math.round(t * 100) / 100);
  // hashes only: what sb-media uses to find scene changes before fetching any pixels
  const hs = await o.ev((a) => globalThis.__sbMedia.frames(a[0], a[1], { hashOnly: true, size: 64 }), [id, times]);
  assert.equal(hs.length, times.length);
  for (const f of hs) {
    assert.match(f.hash, /^[0-9a-f]{19}$/, JSON.stringify(f));
    assert.equal(f.data, undefined);
  }
  const colours = new Set(hs.map((f) => f.hash.slice(0, 3)));
  for (const c of ["f00", "00f", "0f0", "fff"]) assert.ok(colours.has(c), c + " in " + [...colours].join(","));
  // a scene's first frame, with pixels; the same scene sampled again hashes the same
  const red = hs.find((f) => f.hash.startsWith("f00"));
  const fr = await o.ev((a) => globalThis.__sbMedia.frames(a[0], [a[1], a[1] + 0.05], { size: 64 }), [id, red.t]);
  assert.ok(fr[0].data.startsWith("data:image/jpeg"));
  assert.equal(fr[0].w, 64);
  assert.equal(fr[0].h, 48);
  assert.equal(fr[0].hash, red.hash);
  assert.equal(await o.ev(() => document.getElementById("v").currentTime), 0, "playback position restored");
  const one = await o.ev((a) => globalThis.__sbMedia.frame(a[0], a[1]), [id, red.t]);
  assert.equal(one.w, 320, "frame() defaults to 1280, capped at the video size");
});

test("cross-origin video without CORS is reported tainted; captcha video refused", async () => {
  const o = await openMedia();
  const x = await o.ev(() => globalThis.__sb.id(document.getElementById("x")));
  const r = await o.ev((i) => globalThis.__sbMedia.frame(i, 0.5), x);
  assert.match(r.why, /^tainted/);
  const cap = await o.ev(() => globalThis.__sb.id(document.getElementById("cap")));
  const c = await o.ev((i) => globalThis.__sbMedia.frames(i, [0.5]), cap);
  assert.deepEqual(c, [{ t: null, why: "challenge" }]);
});
