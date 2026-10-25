<#
Launch sub-orchestrator sessions, one Windows Terminal tab per area, each in its own git worktree.

  scripts\swarm\launch.ps1 -Areas core,sheet,store,page          # wave 1
  scripts\swarm\launch.ps1 -Areas servers,driver                 # wave 2
  scripts\swarm\launch.ps1 -Areas core -Resume                   # reopen an existing worktree
  scripts\swarm\launch.ps1 -Areas core,page -Restart             # fresh session on an existing worktree,
                                                                  # told to commit WIP, merge main, resume

Requires: git, Windows Terminal (wt), claude on PATH. Run from the repo root on main, with
everything committed. Worktrees go to <drive>:\sb-swarm\wt\<area> (see below).

The prompt is NOT passed on the command line: wt treats ';' as a subcommand separator and
PowerShell 5.1 treats backticks as escapes, and every prompt contains both. Instead each tab
runs a tiny generated launcher (.claude/state/launch.ps1 inside the worktree, git-ignored)
that tells claude to read prompts/suborch/<area>.md from its own worktree.
#>
param(
  [Parameter(Mandatory=$true)][string[]]$Areas,
  [switch]$Resume,
  [switch]$Restart
)
if ($Restart) { $Resume = $true }
$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
Set-Location $root
if ((git rev-parse --abbrev-ref HEAD).Trim() -ne "main") { Write-Error "run launch.ps1 from the repo root on main"; exit 1 }
# Worktrees branch from the committed main, so uncommitted work would silently not reach them.
if (git status --porcelain) { Write-Host "main has uncommitted or untracked files; commit them first (git status)" -ForegroundColor Red; exit 1 }
git cat-file -e "main:.claude/settings.json"
if ($LASTEXITCODE -ne 0) { Write-Host "main has no committed .claude/settings.json; run install-claude-config.ps1 and commit .claude/" -ForegroundColor Red; exit 1 }

# Worktrees live OUTSIDE this tree and the enclosing browser-use repo: Claude Code loads
# CLAUDE.md from every parent directory, so wt/ inside the repo would also load main's
# working-tree CLAUDE.md and F:\aside_mcp for claude\CLAUDE.md. Parent dir is named `wt`
# so scripts/hooks/_common.py area() still resolves the area from the path.
$wtRoot = Join-Path (Split-Path $root -Qualifier) "\sb-swarm\wt"
if (-not (Test-Path $wtRoot)) { New-Item -ItemType Directory -Force $wtRoot | Out-Null }

# -Areas core,sheet arrives as one string when called via `powershell -File`; split it.
$launched = @()
$Areas = $Areas | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ }

foreach ($a in $Areas) {
  $wt = Join-Path $wtRoot $a
  $branch = "swarm/$a"
  if (-not (Test-Path (Join-Path $root "prompts\suborch\$a.md"))) { Write-Warning "no prompt for area '$a'; skipped"; continue }

  if (-not (Test-Path $wt)) {
    git show-ref --verify --quiet "refs/heads/$branch"
    if ($LASTEXITCODE -eq 0) { git worktree add $wt $branch } else { git worktree add -b $branch $wt main }
    if ($LASTEXITCODE -ne 0) { Write-Warning "git worktree add failed for '$a'; skipped"; continue }
    Write-Host "created worktree $wt on $branch"
  } elseif (-not $Resume) {
    Write-Host "worktree $wt exists; pass -Resume to reopen"; continue
  }

  # .claude/ (hooks, agents, commands) and scripts/ come with the checkout; state/ is per worktree.
  $stateDir = Join-Path $wt ".claude\state"
  New-Item -ItemType Directory -Force $stateDir | Out-Null
  $first = "Read prompts/suborch/$a.md completely. It is your launch brief from the supreme orchestrator; follow it as your instructions."
  if ($Restart) {
    # claude --continue finds no transcript in a worktree, so a restart is a fresh session told where it stopped.
    $first += " This is a restart: an earlier session for this area was stopped. Before anything else run git status and git log --oneline main..HEAD, read your newest handoffs in agent-memory/handoffs/ including the supreme comment lines at their end, commit any work in progress you find, merge main into your branch, then continue from the first unfinished step. Use the pr-ready skill before you mark anything pr_open."
  }
  # Temp on the worktree's drive: four sessions' cargo and subagent temp filled C: once (B-405).
  $tmp = Join-Path (Split-Path $wtRoot -Parent) "tmp-$a"
  New-Item -ItemType Directory -Force $tmp | Out-Null
  @(
    "`$env:SB_AREA = '$a'"
    "`$env:TMP = '$tmp'"
    "`$env:TEMP = '$tmp'"
    "`$env:TMPDIR = '$tmp'"
    "Set-Location -LiteralPath '$wt'"
    "claude '$($first -replace "'", "''")'"
  ) | Set-Content -Encoding ASCII (Join-Path $stateDir "launch.ps1")

  # Start-Process with a named window, not `wt -w 0`: from a host that is not a terminal (the VS Code
  # extension) -w 0 has no current window and wt spawns nothing, silently.
  Start-Process wt.exe -ArgumentList @('-w','swarm','new-tab','--title',"swarm:$a",'-d',$wt,'powershell','-NoExit','-ExecutionPolicy','Bypass','-File','.claude\state\launch.ps1')
  $launched += $a
  Start-Sleep -Milliseconds 1500  # wt needs a moment between tab spawns; no event to wait on
}
Start-Sleep -Seconds 8
$running = @(Get-CimInstance Win32_Process -Filter "Name='claude.exe'" | ForEach-Object { if ($_.CommandLine -match 'suborch/(\w+)\.md') { $matches[1] } })
foreach ($a in $launched) {
  if ($running -contains $a) { Write-Host "session up: $a" } else { Write-Host "NOT RUNNING: $a (check its tab in the swarm window)" -ForegroundColor Red }
}
Write-Host "New tabs may wait on the folder-trust prompt until accepted."
Write-Host "`nSupreme stays in $root on main."
