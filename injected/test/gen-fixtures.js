// gen-fixtures.js: writes injected/test/fixtures/register.index.json and register.text.json
// from the live bench/pages/register.html in Chromium (D-405 (10)). The files are committed;
// crates/sb-act/tests/wire_from_injected.rs deserializes them into Vec<sb_sheet::IndexRow> and
// Vec<sb_sheet::TextBlock>, and browser.test.js checks they still match the live output.
// Deterministic: fixed 1280x720 viewport, walk({base: 1}), boxes are integers (index() rounds).
//
//   node injected/test/gen-fixtures.js
"use strict";

const fs = require("node:fs");
const path = require("node:path");
const { serve, launch, openSb } = require("./helpers");

async function main() {
  const srv = await serve();
  const browser = await launch();
  try {
    const o = await openSb(browser, srv.origin + "/register.html");
    const out = await o.ev(() => {
      globalThis.__sb.walk({ base: 1 });
      return { index: globalThis.__sb.index(), text: globalThis.__sb.text() };
    });
    await o.close();
    const dir = path.join(__dirname, "fixtures");
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(path.join(dir, "register.index.json"), JSON.stringify(out.index, null, 1) + "\n");
    fs.writeFileSync(path.join(dir, "register.text.json"), JSON.stringify(out.text, null, 1) + "\n");
    process.stdout.write(`wrote ${out.index.length} rows, ${out.text.length} blocks to ${dir}\n`);
  } finally {
    await browser.close();
    await srv.close();
  }
}

main().catch((e) => {
  process.stderr.write(String(e && e.stack) + "\n");
  process.exit(1);
});
