// @name read_table
// @kind reader
// @match /*
// @args id?:string max?:number
// @gate Read
// @phase 1
// A table on the page as {header, rows}. id is a table id (`3`, `f2.3`); without one, the
// first table on the page. Rows are capped (default 50) and the total is reported.
const DEFAULT_MAX = 50;
const PROBE = 30;

function firstTable(page) {
  for (let i = 0; i < PROBE; i++) {
    const t = page.table(String(i));
    if (t) return { id: String(i), t };
  }
  return null;
}

export default function (page, args) {
  const max = args && args.max > 0 ? Math.floor(args.max) : DEFAULT_MAX;
  let found = null;
  if (args && args.id) {
    const t = page.table(String(args.id));
    if (t) found = { id: String(args.id), t };
  } else {
    found = firstTable(page);
  }
  if (!found) return { error: args && args.id ? 'no table ' + args.id : 'no table on this page' };
  const rows = Array.isArray(found.t.rows) ? found.t.rows : [];
  return {
    id: found.id,
    header: found.t.header || [],
    total: rows.length,
    rows: rows.slice(0, max),
  };
}
