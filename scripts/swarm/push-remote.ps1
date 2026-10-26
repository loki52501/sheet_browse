<#
Push main and every swarm/* branch to GitHub. Run from the repo root on your PC (your git credentials are here).
  scripts\swarm\push-remote.ps1                      # first time: adds origin if missing, pushes everything
  scripts\swarm\push-remote.ps1 -Remote <url>        # override the remote url
#>
param([string]$Remote = "https://github.com/loki52501/sheet_browse.git")
$ErrorActionPreference = "Stop"
$root = (git rev-parse --show-toplevel).Trim(); Set-Location $root
if (-not (git remote | Select-String -Quiet '^origin$')) { git remote add origin $Remote; Write-Host "added origin $Remote" }
else { git remote set-url origin $Remote }
git push -u origin main
foreach ($b in (git for-each-ref --format='%(refname:short)' refs/heads/swarm/)) { git push -u origin $b }
git push origin --tags
Write-Host "`npushed. Open PRs with:  gh pr create --base main --head swarm/<area> --title '<area>: wave N' --fill"
