// @name probe_api
// @kind reader
// @match /*
// @gate Read
// @phase 1
// T-9 sandbox probe for a phase 1 reader: api(url) is present, fetch is not.
export default function (page) {
  return {
    document: typeof document,
    window: typeof window,
    fetch: typeof fetch,
    globalFetch: typeof globalThis.fetch,
    pageFetch: typeof page.fetch,
    pageApi: typeof page.api,
  };
}
