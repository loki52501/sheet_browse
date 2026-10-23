// Test helpers for injected/ (D-401): jsdom loader, a two-port static server for bench/pages,
// and a Playwright + CDP harness that installs injected.js the way sb-driver does
// (Page.addScriptToEvaluateOnNewDocument in world "sb") and evaluates in that world.
"use strict";

const fs = require("node:fs");
const path = require("node:path");
const http = require("node:http");

const ROOT = path.resolve(__dirname, "..", "..");
const PAGES = path.join(ROOT, "bench", "pages");
const SRC = fs.readFileSync(path.join(ROOT, "injected", "injected.js"), "utf8");
const MEDIA_SRC = fs.readFileSync(path.join(ROOT, "injected", "media.js"), "utf8");

function page(name) {
  return fs.readFileSync(path.join(PAGES, name), "utf8");
}

// jsdom with injected.js evaluated in the window (no page scripts run).
function jsdom(html, opts = {}) {
  const { JSDOM } = require("jsdom");
  const dom = new JSDOM(html, {
    runScripts: "outside-only",
    url: opts.url || "http://127.0.0.1/fixture.html",
    pretendToBeVisual: true,
  });
  if (opts.before) opts.before(dom.window);
  dom.window.eval(SRC);
  return { dom, win: dom.window, doc: dom.window.document, sb: dom.window.__sb };
}

// Static server for bench/pages (plus extra in-memory routes) on an ephemeral loopback port.
function serve(extra = {}) {
  return new Promise((resolve) => {
    const server = http.createServer((req, res) => {
      const u = new URL(req.url, "http://x");
      let body;
      let type;
      if (extra[u.pathname]) {
        body = Buffer.from(extra[u.pathname].body);
        type = extra[u.pathname].type || "text/html; charset=utf-8";
      } else {
        const file = path.join(PAGES, path.normalize(u.pathname).replace(/^[/\\]+/, ""));
        if (!file.startsWith(PAGES) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) {
          res.writeHead(404);
          res.end("not found");
          return;
        }
        body = fs.readFileSync(file);
        type = file.endsWith(".html") ? "text/html; charset=utf-8"
          : file.endsWith(".webm") ? "video/webm" : "application/octet-stream";
      }
      // Range support: Chrome only seeks media served with byte ranges.
      const m = /^bytes=(\d*)-(\d*)$/.exec(req.headers.range || "");
      if (m) {
        const start = m[1] ? Number(m[1]) : 0;
        const end = m[2] ? Math.min(Number(m[2]), body.length - 1) : body.length - 1;
        res.writeHead(206, {
          "content-type": type, "accept-ranges": "bytes",
          "content-range": `bytes ${start}-${end}/${body.length}`, "content-length": end - start + 1,
        });
        res.end(body.subarray(start, end + 1));
        return;
      }
      res.writeHead(200, { "content-type": type, "accept-ranges": "bytes", "content-length": body.length });
      res.end(body);
    });
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      resolve({ server, port, origin: `http://127.0.0.1:${port}`, close: () => new Promise((r) => server.close(r)) });
    });
  });
}

async function launch() {
  const { chromium } = require("playwright-core");
  const opts = { headless: true };
  if (process.env.SB_CHROMIUM) opts.executablePath = process.env.SB_CHROMIUM;
  return chromium.launch(opts);
}

// Open url in a fresh context with injected.js (and media.js) in world "sb" of every frame.
async function openSb(browser, url, opts = {}) {
  const context = await browser.newContext({ viewport: opts.viewport || { width: 1280, height: 720 } });
  const pg = await context.newPage();
  const cdp = await context.newCDPSession(pg);
  const worlds = new Map(); // frameId -> executionContextId of world "sb"
  cdp.on("Runtime.executionContextCreated", ({ context: c }) => {
    if (c.name === "sb" && c.auxData && c.auxData.frameId) worlds.set(c.auxData.frameId, c.id);
  });
  cdp.on("Runtime.executionContextDestroyed", ({ executionContextId }) => {
    for (const [f, id] of worlds) if (id === executionContextId) worlds.delete(f);
  });
  cdp.on("Runtime.executionContextsCleared", () => worlds.clear());
  await cdp.send("Runtime.enable");
  await cdp.send("Page.enable");
  await cdp.send("Page.addScriptToEvaluateOnNewDocument", { source: SRC, worldName: "sb" });
  if (opts.media) {
    await cdp.send("Page.addScriptToEvaluateOnNewDocument", { source: MEDIA_SRC, worldName: "sb" });
  }
  await pg.goto(url, { waitUntil: opts.waitUntil || "load" });

  const frameTree = async () => (await cdp.send("Page.getFrameTree")).frameTree;
  const mainId = (await frameTree()).frame.id;

  async function ev(fnOrExpr, arg, frameId) {
    const expression = typeof fnOrExpr === "function"
      ? `(${fnOrExpr})(${JSON.stringify(arg === undefined ? null : arg)})`
      : fnOrExpr;
    const contextId = worlds.get(frameId || mainId);
    if (!contextId) throw new Error("no sb world for frame " + (frameId || mainId));
    const r = await cdp.send("Runtime.evaluate", { expression, contextId, returnByValue: true, awaitPromise: true });
    if (r.exceptionDetails) {
      const e = r.exceptionDetails;
      throw new Error((e.exception && e.exception.description) || e.text);
    }
    return r.result.value;
  }
  async function frames() {
    const out = [];
    const rec = (t, parent) => {
      out.push({ id: t.frame.id, url: t.frame.url, parent });
      for (const c of t.childFrames || []) rec(c, t.frame.id);
    };
    rec(await frameTree(), null);
    return out;
  }
  return { context, page: pg, cdp, ev, frames, mainId, worlds, close: () => context.close() };
}

// Apply sb_sheet's serde defaults to rows as Rust deserializes them (State::default(),
// absent flags = false), so tests can read every field.
const STATE_DEFAULT = { visible: true, enabled: true, submitish: false, opens_new_tab: false, options: [], covered: false };
const fill = (rows) => rows.map((r) => Object.assign({}, r, {
  state: Object.assign({}, STATE_DEFAULT, r.state || {}),
  flags: Object.assign({ submit: false, challenge: false, overlay: false, opaque: false }, r.flags || {}),
}));
const byName = (rows, name, role) => rows.find((r) => r.name === name && (!role || r.role === role));

module.exports = { ROOT, PAGES, SRC, MEDIA_SRC, page, jsdom, serve, launch, openSb, byName, fill };
