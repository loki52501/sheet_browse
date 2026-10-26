// @name list_jobs
// @kind reader
// @match /*
// @args org?:string
// @gate Read
// @phase 1
// Open jobs on an Ashby job board (jobs.ashbyhq.com/<org>) from Ashby's public posting API.
// org defaults to the first path segment of the page URL. Returns short JSON.
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

export default function (page, args) {
  const org = (args && args.org) || orgFromUrl(page.url);
  if (!org) throw new Error('list_jobs: no org given and none in the page URL');
  const board = page.api(apiOrigin(page.url) + '/posting-api/job-board/' + encodeURIComponent(org));
  const all = (board && Array.isArray(board.jobs) ? board.jobs : []).filter((j) => j.isListed !== false);
  const jobs = all.slice(0, MAX_JOBS).map((j) => ({
    title: j.title || '',
    location: placeOf(j) + (j.isRemote ? ' (remote)' : ''),
    department: j.department || j.team || '',
    url: j.jobUrl || j.applyUrl || '',
  }));
  return { org, total: all.length, shown: jobs.length, jobs };
}
