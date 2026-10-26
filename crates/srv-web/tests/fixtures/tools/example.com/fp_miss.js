// @name fp_miss
// @kind reader
// @match /*
// @gate Read
// @fingerprint button"Checkout" input"Coupon code" link"Order history"
// @phase 1
// srv-web W-7 fixture: its fingerprint does not match the shop page (T-4 miss).
export default function (page) {
  return { title: page.title };
}
