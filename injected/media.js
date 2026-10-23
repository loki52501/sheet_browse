// media.js: B-9 in-page video helpers on globalThis.__sbMedia, installed in world "sb" next to
// injected.js. One concern: the facts video.list needs, and the in-page seek + canvas frame
// grab that sb-media uses when a video is not a fetchable file (MSE / blob). DRM videos are
// reported as protected and never drawn; videos in a CHALLENGE frame or container are never
// listed or captured (D-5). Plain ES2020, no dependencies.
//
//   list()                      -> [{id, kind: file|mse|stream|drm, src?, duration, live,
//                                    captions: [{kind, label, lang, mode, cues}], visible,
//                                    box, paused, time, width, height, ready}] | {why}
//   cues(id, track?)            -> [{start, end, text}] | {why}   (loaded text tracks only)
//   frames(id, times, opts)     -> Promise<[{t, w, h, hash, data?} | {t, why, truncated?}]>
//                                  one entry per time, in order. opts: {size=512,
//                                  type="image/jpeg", quality=0.8, hashOnly=false,
//                                  seekCapMs=5000, budgetMs=8000}. Entries not reached within
//                                  budgetMs, or after a seek timed out, are
//                                  {t, why, truncated: true} (D-405 (9), D-423).
//   frame(id, t, opts)          -> Promise<{t, w, h, hash, data} | {why}>
// `id` is the element id from __sb.id(video), so video ids and index ids agree. Ids, element
// lookup and CHALLENGE all come from __sb (D-405 (2)(9)): before the first __sb.walk({base})
// every call returns {why: "no base"}, and without __sb {why: "no __sb"}.
// hash (19 hex chars: mean colour + 64-bit average hash) lets sb-media dedupe frames without
// moving pixels (B-9 "dedupe"); hashOnly skips the image.
(() => {
  "use strict";
  if (globalThis.__sbMedia) return;
  const doc = globalThis.document;
  if (!doc) return;

  const CUES_CAP = 2000;
  const BUDGET_MS = 8000; // under the 10 s command timeout (B-4 #11)
  const SEEK_CAP_MS = 5000;
  const VISIBLE_CONTROL_SEL = 'input:not([type="hidden"]),select,button,a[href]';

  const sb = () => globalThis.__sb;
  const parentOf = (n) => (n.parentNode && n.parentNode.nodeType === 11 && n.parentNode.host ? n.parentNode.host : n.parentNode);
  const contains = (a, b) => {
    for (let n = b; n; n = parentOf(n)) if (n === a) return true;
    return false;
  };

  // Elements covered by __sb's CHALLENGE rows, or a {why} when __sb cannot answer. A
  // challenge row carried on an iframe stands for its widget container too: the iframe's
  // ancestors up to the first one holding a real control (or body, form, main).
  function challengeRoots() {
    const api = sb();
    if (!api) return { why: "no __sb" };
    const rows = api.index();
    if (!Array.isArray(rows)) return rows; // {why: "no base"}
    const roots = [];
    for (const r of rows) {
      if (!r.flags || !r.flags.challenge) continue;
      const el = api.resolve(r.id);
      if (!el || el.nodeType !== 1) continue;
      let top = el;
      if (el.localName === "iframe" || el.localName === "frame") {
        for (let p = parentOf(el); p && p.nodeType === 1; p = parentOf(p)) {
          if (/^(body|html|form|main)$/.test(p.localName) || p.querySelector(VISIBLE_CONTROL_SEL)) break;
          top = p;
        }
      }
      roots.push(top);
    }
    return roots;
  }
  const challenged = (roots, el) => roots.some((c) => contains(c, el));

  function byId(id) {
    const roots = challengeRoots();
    if (!Array.isArray(roots)) return roots;
    const r = sb().resolve(id);
    const el = r && r.nodeType === 1 ? r : null;
    if (!el || !el.isConnected) return { why: (r && r.why) || "gone" };
    if (el.localName !== "video") return { why: "not a video" };
    if (challenged(roots, el)) return { why: "challenge" };
    return el;
  }

  function videos() {
    const out = [];
    const visit = (root) => {
      for (const v of root.querySelectorAll("video")) out.push(v);
      for (const el of root.querySelectorAll("*")) if (el.shadowRoot) visit(el.shadowRoot);
    };
    visit(doc);
    return out;
  }

  function kindOf(v) {
    if (v.mediaKeys) return "drm";
    const so = v.srcObject;
    if (so) return typeof MediaStream !== "undefined" && so instanceof MediaStream ? "stream" : "mse";
    const src = v.currentSrc || v.src || "";
    if (/^blob:/i.test(src)) return "mse";
    return "file";
  }

  function visibleOf(v) {
    const r = v.getBoundingClientRect();
    if (r.width <= 0 || r.height <= 0) return false;
    const cs = getComputedStyle(v);
    return cs.visibility === "visible" && cs.display !== "none";
  }

  function list() {
    const roots = challengeRoots();
    if (!Array.isArray(roots)) return roots;
    return videos().filter((v) => !challenged(roots, v)).map((v) => {
      const kind = kindOf(v);
      const r = v.getBoundingClientRect();
      const d = v.duration;
      const o = {
        id: sb().id(v),
        kind,
        duration: Number.isFinite(d) ? Math.round(d * 1000) / 1000 : null,
        live: d === Infinity,
        captions: Array.from(v.textTracks || []).map((t) => ({
          kind: t.kind, label: t.label, lang: t.language, mode: t.mode, cues: t.cues ? t.cues.length : 0,
        })),
        visible: visibleOf(v),
        box: { x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.width), h: Math.round(r.height) },
        paused: v.paused,
        time: Math.round(v.currentTime * 1000) / 1000,
        width: v.videoWidth,
        height: v.videoHeight,
        ready: v.readyState,
      };
      if (kind === "file") o.src = v.currentSrc || v.src || undefined;
      return o;
    });
  }

  function cues(id, track) {
    const v = byId(id);
    if (v.why) return v;
    const tracks = Array.from(v.textTracks || []);
    const t = typeof track === "number" ? tracks[track] : tracks.find((x) => x.cues && x.cues.length);
    if (!t) return { why: "no text track" };
    if (!t.cues) return { why: "track not loaded (mode " + t.mode + ")" };
    return Array.from(t.cues).slice(0, CUES_CAP).map((c) => ({
      start: c.startTime, end: c.endTime, text: String(c.text || "").replace(/<[^>]*>/g, ""),
    }));
  }

  // Resolve on `name`, reject at the cap: the cap is the deadline of the event, not a sleep.
  function once(target, names, capMs) {
    return new Promise((resolve, reject) => {
      const off = () => {
        clearTimeout(timer);
        for (const n of names) target.removeEventListener(n, on);
        target.removeEventListener("error", onErr);
      };
      const on = () => {
        off();
        resolve();
      };
      const onErr = () => {
        off();
        reject(new Error("media error"));
      };
      for (const n of names) target.addEventListener(n, on, { once: true });
      target.addEventListener("error", onErr, { once: true });
      const timer = setTimeout(() => {
        off();
        reject(new Error("timeout"));
      }, capMs);
    });
  }

  // 19 hex chars: mean colour (3 nibbles, r g b) + 64-bit average hash of an 8x8 grey thumbnail.
  // The colour part separates uniform frames the average hash alone cannot tell apart.
  function frameHash(src) {
    const c = doc.createElement("canvas");
    c.width = 8;
    c.height = 8;
    const x = c.getContext("2d");
    x.drawImage(src, 0, 0, 8, 8);
    const px = x.getImageData(0, 0, 8, 8).data;
    const g = [];
    let r = 0;
    let gr = 0;
    let b = 0;
    for (let i = 0; i < 64; i++) {
      r += px[i * 4];
      gr += px[i * 4 + 1];
      b += px[i * 4 + 2];
      g.push(px[i * 4] * 0.299 + px[i * 4 + 1] * 0.587 + px[i * 4 + 2] * 0.114);
    }
    const mean = g.reduce((a, v) => a + v, 0) / 64;
    let hex = [r, gr, b].map((v) => Math.min(15, Math.round(v / 64 / 17)).toString(16)).join("");
    for (let i = 0; i < 64; i += 4) {
      let nib = 0;
      for (let j = 0; j < 4; j++) nib = (nib << 1) | (g[i + j] > mean ? 1 : 0);
      hex += nib.toString(16);
    }
    return hex;
  }

  function seekable(v, t) {
    const r = v.seekable;
    if (!r) return true;
    for (let i = 0; i < r.length; i++) if (t >= r.start(i) - 0.001 && t <= r.end(i) + 0.001) return true;
    return false;
  }

  async function grab(v, t, opts, cap) {
    // cap() is the time left for the next event: min(seekCapMs, what remains of the budget).
    if (v.readyState < 1) {
      const p = once(v, ["loadedmetadata"], cap());
      // preload=none: nothing loads until asked.
      if (v.readyState === 0 && v.networkState !== 2) v.load();
      await p;
    }
    const dur = Number.isFinite(v.duration) ? v.duration : null;
    const target = dur !== null ? Math.min(Math.max(0, t), Math.max(0, dur - 0.001)) : Math.max(0, t);
    if (Math.abs(v.currentTime - target) > 0.001) {
      if (!seekable(v, target)) return { t: target, why: "not seekable (no byte ranges or live)" };
      const p = once(v, ["seeked"], cap());
      v.currentTime = target;
      await p;
      if (Math.abs(v.currentTime - target) > 0.25) return { t: target, why: "seek did not land" };
    }
    if (v.readyState < 2) await once(v, ["loadeddata", "canplay"], cap());
    const vw = v.videoWidth;
    const vh = v.videoHeight;
    if (!vw || !vh) return { t: target, why: "no video frame" };
    const size = opts.size || 512;
    const k = Math.min(1, size / Math.max(vw, vh));
    const w = Math.max(1, Math.round(vw * k));
    const h = Math.max(1, Math.round(vh * k));
    const c = doc.createElement("canvas"); // detached: never visible to the site
    c.width = w;
    c.height = h;
    const ctx = c.getContext("2d");
    ctx.drawImage(v, 0, 0, w, h);
    let hash;
    let data;
    try {
      hash = frameHash(c);
      if (!opts.hashOnly) data = c.toDataURL(opts.type || "image/jpeg", opts.quality || 0.8);
    } catch (e) {
      return { t: target, why: "tainted (cross-origin video without CORS)" };
    }
    const o = { t: Math.round(v.currentTime * 1000) / 1000, w, h, hash };
    if (data) o.data = data;
    return o;
  }

  async function frames(id, times, opts) {
    opts = opts || {};
    const v = byId(id);
    if (v.why) return [{ t: null, why: v.why }];
    if (kindOf(v) === "drm") return [{ t: null, why: "protected (DRM)" }];
    const deadline = performance.now() + Number(opts.budgetMs || BUDGET_MS);
    const seekCap = Number(opts.seekCapMs || SEEK_CAP_MS);
    const saved = { time: v.currentTime, paused: v.paused };
    if (!v.paused) v.pause();
    const out = [];
    const ts = Array.from(times || [], Number);
    let stop = "";
    try {
      for (const t of ts) {
        const left = deadline - performance.now();
        if (!stop && left <= 0) stop = "time budget";
        if (stop) {
          out.push({ t, why: stop, truncated: true });
          continue;
        }
        try {
          out.push(await grab(v, t, opts, () => Math.max(1, Math.min(seekCap, deadline - performance.now()))));
        } catch (e) {
          const timeout = e && e.message === "timeout";
          out.push({ t, why: timeout ? "seek timed out" : "media error" });
          // A seek that timed out will time out again: the rest would only burn the budget.
          if (timeout) stop = "skipped after a seek timed out";
        }
      }
    } finally {
      try {
        if (Math.abs(v.currentTime - saved.time) > 0.001) v.currentTime = saved.time;
        if (!saved.paused) v.play().catch(() => undefined);
      } catch (e) {
        // restoring playback state is best effort
      }
    }
    return out;
  }

  async function frame(id, t, opts) {
    const r = await frames(id, [t], Object.assign({ size: 1280 }, opts || {}));
    return r[0];
  }

  Object.defineProperty(globalThis, "__sbMedia", {
    value: Object.freeze({ list, cues, frames, frame }),
    configurable: false,
    enumerable: false,
    writable: false,
  });
})();
