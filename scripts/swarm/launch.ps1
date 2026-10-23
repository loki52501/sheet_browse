<#
Launch sub-orchestrator sessions, one Windows Terminal tab per area, each in its own git worktree.

  scripts\swarm\launch.ps1 -Areas core,sheet,store,page          # wave 1
  scripts\swarm\launch.ps1 -Areas servers,driver                 # wave 2
  scripts\swarm\launch.ps1 -Areas core -Resume                   # reopen an existing worktree

Requires: git, Windows Terminal (wt), claude on PATH. Run from the repo root on main.
#>
param(
  [Parameter(Mandatory=$true)][string[]]$Areas,
  [switch]$Resume
)
$ErrorActionPreference = "Stop"
$root = (git rev-parse --show-toplevel).Trim()
Set-Location $root
if (-not (Test-Path "$root\wt")) { New-Item -ItemType Directory "$root\wt" | Out-Null }

foreach ($a in $Areas) {
  $wt = Join-Path $root "wt\$a"
  $branch = "swarm/$a"
  $prompt = Join-Path $root "prompts\suborch\$a.md"
  if (-not (Test-Path $prompt)) { Write-Error "no prompt for area '$a' at $prompt"; continue }

  if (-not (Test-Path $wt)) {
    if (git branch --list $branch) { git worktree add $wt $branch } else { git worktree add -b $branch $wt main }
    Write-Host "created worktree $wt on $branch"
  } elseif (-not $Resume) {
    Write-Host "worktree $wt exists; pass -Resume to reopen"; continue
  }

  # each worktree shares .claude/ and scripts/ via git, so hooks and agents are present
  $text = Get-Content $prompt -Raw
  $escaped = $text.Replace('"','\"')
  $cmd = "`$env:SB_AREA='$a'; Set-Location '$wt'; claude `"$escaped`""
  wt -w 0 new-tab --title "swarm:$a" powershell -NoExit -Command $cmd
  Start-Sleep -Milliseconds 800
}
Write-Host "`nSupreme: stay in $root on main and run:  claude `"$(Get-Content prompts\SUPREME.md -Raw | Select-Object -First 1)...`""
