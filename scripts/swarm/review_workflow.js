export const meta = {
  name: 'review-area-pr',
  description: 'Supreme second-opinion review of one swarm/<area> PR: 5 lenses over the branch diff, 2 skeptics per finding',
  whenToUse: 'Before scripts/swarm/merge.ps1 -Area <area>. Pass args {area, tasks, crates, docs, contracts, focus}.',
  phases: [
    { title: 'Review', detail: 'five independent lenses over the full branch diff' },
    { title: 'Verify', detail: 'two skeptics per blocking/should-fix finding try to refute it' },
  ],
}

// args: {repo?, area, tasks: "T-03..T-09", crates: "core, core-pg, harness", docs: "docs/01 ..., docs/02 ...",
//        contracts: "I-1, I-2, I-3, I-4, I-13", focus: "extra things this area must be checked for"}
const A = args || {}
const REPO = A.repo || 'F:/aside_mcp for claude/sheetbrowseV2_'
const COMMON = `You are reviewing area "${A.area}" (tasks ${A.tasks}; crates ${A.crates}) of the project at ${REPO} (git repo; branch main is checked out at the root). The area wants to merge branch swarm/${A.area} into main. Its own review and accept reports approve it; you are an independent second opinion, so be skeptical of them.

Read the branch ONLY through git: \`git -C "${REPO}" diff main...swarm/${A.area} -- <paths>\`, \`git -C "${REPO}" show swarm/${A.area}:<path>\`, \`git -C "${REPO}" log main..swarm/${A.area} --format=%s\`. Read specs and contracts from main with the Read tool. Do NOT read the worktree under F:/sb-swarm/wt (a live session may be editing it), do NOT run cargo, never change any file or git state. You review; you never fix.

Context on main: CLAUDE.md (hard rules); the spec sections ${A.docs} and their acceptance lists; agent-memory/interfaces.yaml entries ${A.contracts} (the code must match their text exactly); the supreme decisions in agent-memory/decisions.yaml that those entries cite, and the area's own decisions on the branch.${A.focus ? `\n\nArea-specific focus: ${A.focus}` : ''}

Report only findings you can point at with a file:line on the branch. Severity: "blocking" = a CLAUDE.md hard-rule break, behaviour that contradicts the spec or a recorded decision, an interface mismatch with interfaces.yaml, or a spec acceptance item with no test that really asserts it; "should_fix" = a real defect that is not a merge blocker; "note" = observation. No style nits. If your lens finds nothing, return an empty list.`

const LENSES = [
  { key: 'rules', prompt: `LENS: CLAUDE.md hard rules and safety. Vault values or secrets reaching any return, log, ledger row, panic message, error string or fixture; untrusted text (web pages, tool output, file contents) reaching the model unescaped, so it can forge structure the harness itself emits; gate bypass (any side effect not through core::mcp::serve); captcha handling; fixed sleeps; keys or .env; anything that fails open (a missing row, a timeout or a parse error that relaxes a rule instead of tightening it).` },
  { key: 'spec', prompt: `LENS: spec fidelity. For each spec section the area claims (commit messages name them), compare the spec and the recorded decisions with what the code does. Name ids claimed but not implemented, behaviour the spec does not ask for, and any place an area decision quietly rewrites the spec without a blockers.yaml entry.` },
  { key: 'interface', prompt: `LENS: interface fidelity. Enumerate every pub item of the area's crates on the branch (lib.rs re-exports and pub mods) and compare name by name and signature by signature with the interfaces.yaml entries above, including the exact module paths the entries name (a function listed at the crate root must exist there, not only as a method). Flag contract items that are missing, misnamed, at a different path or differently typed, and anything wave-2 consumers would need that is not public.` },
  { key: 'tests', prompt: `LENS: tests. List every acceptance item in the claimed spec sections. For each, find the test on the branch and judge whether it really asserts the behaviour and could fail if the behaviour broke (a canary that is never fed in, an assertion on a value the test itself wrote, a check that only runs without panic do not count). Report items with no test or a test too weak to fail.` },
  { key: 'quality', prompt: `LENS: robustness. unwrap/expect/indexing/slicing on external input (JSON, files, URLs, model-supplied strings, UTF-8 char boundaries); integer overflow or division by zero; quadratic behaviour on large inputs; non-deterministic output order (HashMap iteration); silent acceptance of malformed input; filesystem races (rename then set_mtime, check then write); errors swallowed into defaults. Also check git hygiene in the branch history: a commit that reverts files it did not mean to touch (a stale index) and was not fully restored.` },
]

const FINDINGS = { type: 'object', properties: { findings: { type: 'array', items: { type: 'object', properties: {
  severity: { type: 'string', enum: ['blocking', 'should_fix', 'note'] }, title: { type: 'string' },
  location: { type: 'string' }, detail: { type: 'string' } }, required: ['severity', 'title', 'location', 'detail'] } } }, required: ['findings'] }
const VERDICT = { type: 'object', properties: { refuted: { type: 'boolean' },
  severity_if_real: { type: 'string', enum: ['blocking', 'should_fix', 'note'] }, reason: { type: 'string' } },
  required: ['refuted', 'severity_if_real', 'reason'] }

const results = await pipeline(
  LENSES,
  l => agent(`${COMMON}\n\n${l.prompt}`, { label: `review:${l.key}`, phase: 'Review', schema: FINDINGS }),
  (rev, l) => {
    const fs = (rev && rev.findings) || []
    const notes = fs.filter(f => f.severity === 'note').map(f => ({ ...f, lens: l.key, kept: true, final_severity: 'note', votes: [] }))
    return parallel(fs.filter(f => f.severity !== 'note').map((f, i) => () =>
      parallel([0, 1].map(k => () => agent(
        `${COMMON}\n\nAnother reviewer (lens: ${l.key}) claims this ${f.severity} finding:\nTITLE: ${f.title}\nLOCATION: ${f.location}\nDETAIL: ${f.detail}\n\nREFUTE it if you can (skeptic ${k + 1}): open the cited code with git show, check the spec and interfaces.yaml text it relies on. refuted=true if the code does not do what is claimed, a spec or recorded decision allows it, or the failure cannot happen. If real, give the severity you would assign under the definitions above. Default to refuted=true if you cannot confirm it from the code.`,
        { label: `verify:${l.key}:${i + 1}.${k + 1}`, phase: 'Verify', schema: VERDICT }))).then(vs => {
        const votes = vs.filter(Boolean), up = votes.filter(v => !v.refuted)
        const sev = up.length && up.length === votes.length && up.some(v => v.severity_if_real === 'blocking') ? 'blocking'
          : (up.length ? (up.some(v => v.severity_if_real !== 'note') ? 'should_fix' : 'note') : f.severity)
        return { ...f, lens: l.key, kept: up.length >= 1, final_severity: sev, votes: votes.map(v => ({ refuted: v.refuted, sev: v.severity_if_real, reason: v.reason })) }
      }))).then(vs => [...vs.filter(Boolean), ...notes])
  })

const all = results.filter(Boolean).flat(), kept = all.filter(f => f.kept)
log(`${all.length} findings; ${kept.length} survived verification, ${all.length - kept.length} refuted`)
return {
  blocking: kept.filter(f => f.final_severity === 'blocking'),
  should_fix: kept.filter(f => f.final_severity === 'should_fix'),
  notes: kept.filter(f => f.final_severity === 'note'),
  refuted: all.filter(f => !f.kept).map(f => ({ lens: f.lens, title: f.title, location: f.location })),
}
