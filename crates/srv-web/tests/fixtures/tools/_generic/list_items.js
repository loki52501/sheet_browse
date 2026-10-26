// @name list_items
// @kind reader
// @match /*
// @gate Read
// @phase 1
// srv-web W-7 fixture: shadowed by example.com/list_items.js on example.com pages (T-3.4).
export default function () {
  return { generic: true };
}
