// @name needs_browser
// @kind reader
// @match /*
// @gate Read
// @phase 2
// srv-web W-7 fixture: a @phase 2 reader is not registered by srv-web.
export default function (page) {
  return { title: page.title };
}
