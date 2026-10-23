<#
Supreme helper: merge an area branch into main if the gate passes (docs/10 S-8).
  scripts\swarm\merge.ps1 -Area core

Order: merge --no-commit (so the branch's own review/accept reports are in the tree),
abort on conflicts, run scripts/swarm/merge_gate.py for every task the area owns,
run `cargo test --workspace` (Postgres from docker-compose.yml must be up), then commit.
Any failure aborts the merge and leaves main untouched.
#>
param([Parameter(Mandatory=$true)][string]$Area)
$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path; Set-Location $root
$branch = "swarm/$Area"
function Fail([string]$msg, [switch]$Abort) {
  # no 2> redirect: in PS 5.1 that turns git's stderr into a terminating error under EAP=Stop
  if ($Abort) { git merge --abort | Out-Null }
  Write-Host "MERGE BLOCKED ($branch): $msg" -ForegroundColor Red
  exit 1
}

git show-ref --verify --quiet "refs/heads/$branch"; if ($LASTEXITCODE -ne 0) { Fail "no local branch $branch" }
if ((git rev-parse --abbrev-ref HEAD).Trim() -ne "main") { Fail "run from main" }
if (git status --porcelain --untracked-files=no) { Fail "main has uncommitted changes; commit or stash first" }
if (git remote) { git fetch --all --quiet }

git merge --no-ff --no-commit $branch
$mergeExit = $LASTEXITCODE
$conflicts = git diff --name-only --diff-filter=U
if ($conflicts) { Fail "conflicts in: $($conflicts -join ', '). Resolve on $branch (merge main into it) and retry." -Abort }
if ($mergeExit -ne 0 -or -not (Test-Path (Join-Path (git rev-parse --git-dir) "MERGE_HEAD"))) {
  Fail "git merge failed or there is nothing to merge (see git's message above)"
}

# the area's worktree, for the leak-log check (launch.ps1 puts it outside this tree)
$wtPath = $null
$lines = git worktree list --porcelain
for ($i = 0; $i -lt $lines.Count; $i++) {
  if ($lines[$i] -eq "branch refs/heads/$branch") { $wtPath = ($lines[$i - 2] -replace '^worktree ', '') }
}
if ($wtPath) { python scripts/swarm/merge_gate.py $Area --worktree $wtPath } else { python scripts/swarm/merge_gate.py $Area }
if ($LASTEXITCODE -ne 0) { Fail "review/accept/leak gate failed (see above)" -Abort }

cargo test --workspace --quiet
if ($LASTEXITCODE -ne 0) { Fail "cargo test --workspace failed on the merge result" -Abort }
git add Cargo.lock  # cargo may have re-resolved a lock merged from two branches

git commit -q -m "merge $branch into main (wave gate passed)"
if ($LASTEXITCODE -ne 0) { Fail "git commit failed" -Abort }
Write-Host "merged $branch. Next: agent-memory/swarm.yaml areas.$Area.merged = true, open the next wave, tell waiting areas to merge main."
