// @name list_events
// @kind reader
// @match /*
// @args city?:string
// @gate Read
// @forged 2026-10-01 task=T-31 by=toolforge score=0.90
// @phase 1
export default function (page, args) {
  return { url: page.url, city: (args && args.city) || '' };
}
