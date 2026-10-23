// @name read_table
// @kind reader
// @match /*
// @args id?:string max?:number
// @gate Read
// @phase 1
// A table on the page as {header, rows}. id is a table id (`3`, `f2.3`); without one, the
// first table on the page. Output is short text (rule 5): at most 50 rows (max may lower
// it, never raise it), 20 columns, 200 UTF-16 units per cell (cut on a code point
// boundary) and 8000 characters in all; `truncated` says whether anything was cut, and
// `total` / `columns` give the full size.
const MAX_ROWS = 50;
const MAX_COLS = 20;
const MAX_CELL = 200;
// Total characters across all shown cells; rows stop before this is passed.
const MAX_CHARS = 8000;
const PROBE = 30;

function firstTable(page) {
  for (let i = 0; i < PROBE; i++) {
    const t = page.table(String(i));
    if (t) return { id: String(i), t };
  }
  return null;
}

export default function (page, args) {
  const want = args && args.max > 0 ? Math.floor(args.max) : MAX_ROWS;
  const max = Math.min(want, MAX_ROWS);
  let found = null;
  if (args && args.id) {
    const t = page.table(String(args.id));
    if (t) found = { id: String(args.id), t };
  } else {
    found = firstTable(page);
  }
  if (!found) return { error: args && args.id ? 'no table ' + args.id : 'no table on this page' };
  const allRows = Array.isArray(found.t.rows) ? found.t.rows : [];
  const allHeader = Array.isArray(found.t.header) ? found.t.header : [];
  let truncated = allRows.length > max;
  let columns = allHeader.length;
  for (const r of allRows) {
    if (Array.isArray(r) && r.length > columns) columns = r.length;
  }
  if (columns > MAX_COLS) truncated = true;
  const cell = (v) => {
    const s = v == null ? '' : String(v);
    if (s.length <= MAX_CELL) return s;
    truncated = true;
    // Never split a surrogate pair: a lone high surrogate is not valid JSON text.
    let cut = MAX_CELL - 1;
    const last = s.charCodeAt(cut - 1);
    if (last >= 0xd800 && last <= 0xdbff) cut -= 1;
    return s.slice(0, cut) + '…';
  };
  const line = (r) => {
    const cells = Array.isArray(r) ? r : [r];
    return cells.slice(0, MAX_COLS).map(cell);
  };
  const header = line(allHeader);
  let used = header.reduce((n, c) => n + c.length, 0);
  const rows = [];
  for (const r of allRows.slice(0, max)) {
    const l = line(r);
    used += l.reduce((n, c) => n + c.length, 0);
    if (used > MAX_CHARS) {
      truncated = true;
      break;
    }
    rows.push(l);
  }
  return {
    id: found.id,
    header,
    total: allRows.length,
    columns,
    rows,
    truncated,
  };
}
