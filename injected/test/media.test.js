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

// Under load the recorder can drop most frames, so a clip is checked (every colour present,
// at least 2 s long) and recorded again, at most 3 times.
async function goodClip(br) {
  let last = null;
  for (let i = 0; i < 3; i++) {
    const buf = await recordClip(br);
    last = await clipFacts(br, buf);
    if (last.duration >= 2 && ["f00", "00f", "0f0", "fff"].every((c) => last.colours.includes(c))) return buf;
  }
  throw new Error("could not record a usable clip: " + JSON.stringify(last));
}

// Duration and mean colours (3 nibbles, as media.js frameHash) of a clip, played from a blob.
async function clipFacts(br, buf) {
  const ctx = await br.newContext();
  try {
    const pg = await ctx.newPage();
    return await pg.evaluate(async (b64) => {
      const bytes = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
      const v = document.createElement("video");
      v.muted = true;
      const on = (name) => new Promise((res, rej) => {
        // the cap is the deadline of the media event
        const t = setTimeout(() => rej(new Error("timeout " + name)), 5000);
        v.addEventListener(name, () => { clearTimeout(t); res(); }, { once: true });
      });
      const meta = on("loadeddata");
      v.src = URL.createObjectURL(new Blob([bytes], { type: "video/webm" }));
      await meta;
      const c = document.createElement("canvas");
      c.width = 8;
      c.height = 8;
      const x = c.getContext("2d");
      const colours = new Set();
      for (let t = 0.05; t < v.duration; t += 0.1) {
        const s = on("seeked");
        v.currentTime = t;
        await s;
        x.drawImage(v, 0, 0, 8, 8);
        const px = x.getImageData(0, 0, 8, 8).data;
        const m = [0, 1, 2].map((k) => { let a = 0; for (let i = 0; i < 64; i++) a += px[i * 4 + k]; return a / 64; });
        colours.add(m.map((a) => Math.min(15, Math.round(a / 17)).toString(16)).join(""));
      }
      return { duration: v.duration, colours: [...colours] };
    }, buf.toString("base64"));
  } catch (e) {
    return { duration: 0, colours: [], error: String(e && e.message) };
  } finally {
    await ctx.close();
  }
}

async function recordClip(br) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "sb-media-"));
  const ctx = await br.newContext({ viewport: { width: 320, height: 240 }, recordVideo: { dir, size: { width: 320, height: 240 } } });
  const pg = await ctx.newPage();
  // Four colour segments by the page's own clock: each lasts 600 ms and at least 20 painted
  // frames, so a slow machine paints longer segments instead of skipping them. The test waits
  // on the page's flag.
  await pg.setContent(`<body style="margin:0;background:#f00"><script>
    const cs = ["#f00", "#00f", "#0f0", "#fff"]; let i = 0; let t0 = performance.now(); let n = 0;
    (function step() {
      if (performance.now() - t0 >= 600 && n >= 20) { i++; t0 = performance.now(); n = 0; }
      if (i >= cs.length) { document.body.dataset.done = "1"; return; }
      n++;
      document.body.style.background = cs[i]; requestAnimationFrame(step);
    })();
  </script></body>`);
  // 2.4 s of painting; the cap is generous because other test runs may share the machine.
  try {
    await pg.waitForFunction(() => document.body.dataset.done === "1", null, { timeout: 30000 });
  } catch (e) {
    await ctx.close().catch(() => undefined); // a recording context left open keeps the run alive
    throw e;
  }
  const v = pg.video();
  await ctx.close();
  return fs.readFileSync(await v.path());
}

const PAGE = (other) => `<!doctype html><html><body>
  <video id="v" src="/clip.webm" muted width="320" height="240"></video>
  <video id="x" src="${other}/clip.webm" muted width="160" height="120"></video>
  <div class="g-recaptcha"><video id="cap" src="/clip.webm" muted></video></div>
  <video id="pn" src="/clip.webm" muted preload="none" width="160" height="120"></video>
</body></html>`;

before(async () => {
  browser = await launch();
  clip = await goodClip(browser);
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

async function openMedia(opts = {}) {
  const o = await openSb(browser, A.origin + "/media.html", { media: true });
  opened.push(o);
  await o.page.waitForFunction(() => ["v", "x"].every((i) => document.getElementById(i).readyState >= 2), null, { timeout: 10000 });
  // sb-driver's first based walk (D-405 (2)); media.js never issues ids before it
  if (opts.walk !== false) await o.ev(() => globalThis.__sb.walk({ base: 1 }));
  return o;
}

test("video.list facts: kind, duration, src, visible, ids shared with __sb; captcha video excluded", async () => {
  const o = await openMedia();
  assert.equal(await o.page.evaluate(() => typeof window.__sbMedia), "undefined");
  const list = await o.ev(() => globalThis.__sbMedia.list());
  assert.equal(list.length, 3, JSON.stringify(list));
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

test("no ids before the first based walk; CHALLENGE comes from __sb rows (D-405 (2)(9))", async () => {
  const o = await openMedia({ walk: false });
  assert.deepEqual(await o.ev(() => globalThis.__sbMedia.list()), { why: "no base" });
  assert.deepEqual(await o.ev(() => globalThis.__sbMedia.cues(1)), { why: "no base" });
  assert.deepEqual(await o.ev(() => globalThis.__sbMedia.frames(1, [0.1])), [{ t: null, why: "no base" }]);
  await o.ev(() => globalThis.__sb.walk({ base: 700 }));
  const list = await o.ev(() => globalThis.__sbMedia.list());
  assert.ok(list.length === 3 && list.every((v) => v.id >= 700), JSON.stringify(list));
  const cap = await o.ev(() => globalThis.__sb.id(document.getElementById("cap")));
  assert.ok(!list.some((v) => v.id === cap), "the video inside the CHALLENGE container is not listed");
  // the same container around a challenge iframe: the row is carried on the iframe, the video is still refused
  await o.page.evaluate(() => {
    const f = document.createElement("iframe");
    f.title = "reCAPTCHA";
    f.srcdoc = "<p>x</p>";
    document.querySelector(".g-recaptcha").prepend(f);
  });
  await o.ev(() => globalThis.__sb.walk({ dirty: true }));
  assert.deepEqual(await o.ev((i) => globalThis.__sbMedia.frames(i, [0.5]), cap), [{ t: null, why: "challenge" }]);
});

test("frames(): total budget with truncated entries aligned to times; preload=none loads", async (t) => {
  const o = await openMedia();
  const id = await o.ev(() => globalThis.__sb.id(document.getElementById("v")));
  const times = [];
  for (let i = 0; i < 400; i++) times.push((i % 20) * 0.1 + 0.05);
  const r = await o.ev(async (a) => {
    const t0 = performance.now();
    const out = await globalThis.__sbMedia.frames(a[0], a[1], { hashOnly: true, size: 32, budgetMs: 400 });
    return { out, ms: performance.now() - t0 };
  }, [id, times]);
  t.diagnostic(`frames budget 400 ms: ${r.ms.toFixed(0)} ms, ${r.out.filter((f) => f.hash).length} grabbed of ${times.length}`);
  assert.equal(r.out.length, times.length, "one entry per time");
  assert.ok(r.ms < 400 + 600, `budget overrun: ${r.ms.toFixed(0)} ms`);
  const cut = r.out.findIndex((f) => f.truncated);
  assert.ok(cut > 0, "some frames were grabbed and the rest truncated");
  assert.ok(r.out.slice(cut).every((f) => f.truncated && f.why), "every entry after the cut is truncated");
  assert.ok(r.out.slice(0, cut).every((f) => f.hash || f.why));
  assert.deepEqual(r.out.slice(cut).map((f) => f.t), times.slice(cut));
  // a preload=none video has nothing loaded until frames() asks for it
  const pn = await o.ev(() => globalThis.__sb.id(document.getElementById("pn")));
  assert.equal(await o.ev(() => document.getElementById("pn").readyState), 0);
  const f = await o.ev((i) => globalThis.__sbMedia.frames(i, [0.5], { hashOnly: true, size: 32 }), pn);
  assert.match(f[0].hash || "", /^[0-9a-f]{19}$/, JSON.stringify(f));
});
