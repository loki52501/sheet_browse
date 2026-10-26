// @name list_items
// @kind reader
// @match /shop/*
// @args max?:number
// @gate Read
// @fingerprint button"Add to cart"
// @phase 1
// srv-web W-7 fixture: reads the fetched index and calls page.api on its own site.
export default function (page, args) {
  const button = page.find('button "Add to cart"');
  const stock = page.api('http://api.example.com/stock');
  return { title: page.title, url: page.url, button, stock: stock.count, max: (args && args.max) || null };
}
