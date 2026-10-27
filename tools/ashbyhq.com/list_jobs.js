// @name list_jobs
// @kind reader
// @match /*
// @args org?:string offset?:number q?:string
// @gate Read
// @phase 1
// Open jobs on an Ashby job board (jobs.ashbyhq.com/<org>), 60 per call. q keeps jobs whose
// title, department or team contains it (any case); offset skips that many matching jobs,
// so pass next_offset to get the rest. org defaults to the first path segment of the page URL.
// Returns {org, total, matched, offset, shown, next_offset?, jobs: [{title, location,
// department, url}]}.
//
// The API call uses the page's scheme (D-561): boards are https, so production calls
// https://api.ashbyhq.com; an offline test that serves the board over http stays on http.
const MAX_JOBS = 60;

function orgFromUrl(url) {
  const m = /^[a-z]+:\/\/[^/]+\/([^/?#]+)/i.exec(url || '');
  return m ? decodeURIComponent(m[1]) : '';
}

function apiOrigin(url) {
  return /^http:\/\//i.test(url || '') ? 'http://api.ashbyhq.com' : 'https://api.ashbyhq.com';
}

function placeOf(job) {
  if (job.location) return job.location;
  const addr = job.address && job.address.postalAddress;
  if (!addr) return '';
  return [addr.addressLocality, addr.addressRegion, addr.addressCountry].filter(Boolean).join(', ');
}

function offsetOf(v) {
  if (v === undefined || v === null || v === '') return 0;
  const n = Number(v);
  if (!Number.isFinite(n) || n < 0 || Math.floor(n) !== n) {
    throw new Error('list_jobs: offset must be a whole number of jobs, 0 or more');
  }
  return n;
}

function matches(job, needle) {
  if (!needle) return true;
  return [job.title, job.department, job.team].some(
    (f) => typeof f === 'string' && f.toLowerCase().includes(needle),
  );
}

export default function (page, args) {
  const a = args || {};
  const org = a.org || orgFromUrl(page.url);
  if (!org) throw new Error('list_jobs: no org given and none in the page URL');
  const offset = offsetOf(a.offset);
  const needle = typeof a.q === 'string' ? a.q.trim().toLowerCase() : '';
  const board = page.api(apiOrigin(page.url) + '/posting-api/job-board/' + encodeURIComponent(org));
  const all = (board && Array.isArray(board.jobs) ? board.jobs : []).filter((j) => j && j.isListed !== false);
  const hits = all.filter((j) => matches(j, needle));
  const jobs = hits.slice(offset, offset + MAX_JOBS).map((j) => ({
    title: j.title || '',
    location: placeOf(j) + (j.isRemote ? ' (remote)' : ''),
    department: j.department || j.team || '',
    url: j.jobUrl || j.applyUrl || '',
  }));
  const out = { org, total: all.length, matched: hits.length, offset, shown: jobs.length };
  if (offset + jobs.length < hits.length) out.next_offset = offset + jobs.length;
  out.jobs = jobs;
  return out;
}
