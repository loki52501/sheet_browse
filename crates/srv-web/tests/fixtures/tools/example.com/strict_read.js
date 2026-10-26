// @name strict_read
// @kind reader
// @match /*
// @gate Submit
// @phase 1
// srv-web W-7 fixture: a reader with a raised gate floor registers at that class.
export default function (page) {
  return { title: page.title };
}
