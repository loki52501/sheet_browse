<#
Supreme helper: merge an area branch into main if the gate passes.
  scripts\swarm\merge.ps1 -Area core
Checks: review report without "## Blocking" items, accept reports green, workspace tests pass on a temp merge.
#>
param([Parameter(Mandatory=$true)][string]$Area)
$ErrorActionPreference = "Stop"
$root = (git rev-parse --show-toplevel).Trim(); Set-Location $root
$branch = "swarm/$Area"
git fetch --all | Out-Null

$reviews = Get-ChildItem bench\results\review-*.md -ErrorAction SilentlyContinue
foreach ($r in $reviews) {
  $txt = Get-Content $r.FullName -Raw
  if ($txt -match '(?s)## Blocking\s*\n(?!\s*(none|-\s*none|$))\S') { Write-Error "review $($r.Name) has blocking items"; exit 1 }
}
$accepts = Get-ChildItem bench\results\accept-*.md -ErrorAction SilentlyContinue
foreach ($x in $accepts) { if ((Get-Content $x.FullName -Raw) -match '(?i)\bfail\b') { Write-Error "accept $($x.Name) has failures"; exit 1 } }

git checkout -q main
git merge --no-ff --no-commit $branch
cargo test --workspace --quiet
if ($LASTEXITCODE -ne 0) { git merge --abort; Write-Error "workspace tests failed on merge"; exit 1 }
git commit -m "merge $branch into main (wave gate passed)"
Write-Host "merged $branch. Update agent-memory/swarm.yaml: areas.$Area.merged = true, and tell waiting areas to pull."
