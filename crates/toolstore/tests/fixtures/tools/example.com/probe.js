// @name probe
// @kind reader
// @match /*
// @gate Read
// @phase 2
// T-9 sandbox probe: reports what a tool can see. Never returns page content.
function attempt(f) {
  try {
    return 'ran:' + String(f());
  } catch (e) {
    return 'threw:' + (e && e.name);
  }
}

export default function (page) {
  return {
    document: typeof document,
    window: typeof window,
    fetch: typeof fetch,
    globalFetch: typeof globalThis.fetch,
    xhr: typeof XMLHttpRequest,
    require: typeof require,
    eval: typeof eval,
    setTimeout: typeof setTimeout,
    pageDocument: typeof page.document,
    pageWindow: typeof page.window,
    pageFetch: typeof page.fetch,
    pageApi: typeof page.api,
    pageKeys: Object.getOwnPropertyNames(page).sort(),
    pageFrozen: Object.isFrozen(page),
    fnCtor: attempt(() => (function () {}).constructor('return 1')()),
    globalFn: attempt(() => Function('return 1')()),
  };
}
