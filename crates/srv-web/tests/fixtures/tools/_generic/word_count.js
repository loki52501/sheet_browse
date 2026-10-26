// @name word_count
// @kind reader
// @match /*
// @gate Read
// @phase 1
// srv-web W-7 fixture: a site-agnostic reader, registered as generic.word_count.
export default function (page) {
  const words = page.text().split(/\s+/).filter((w) => w.length > 0);
  return { words: words.length };
}
