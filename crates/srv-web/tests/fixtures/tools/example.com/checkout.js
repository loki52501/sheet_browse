// @name checkout
// @kind planner
// @match /*
// @args qty:string
// @gate Submit
// @phase 1
// srv-web W-7 fixture: planners need the browser's act(); srv-web does not register them.
export default function (page, args) {
  const qty = page.find('input "Quantity"');
  const btn = page.find('button "Add to cart"');
  return `t#${qty}=${q(args.qty)}; ?s; c#${btn}`;
}
