# Copies claude-config/ into .claude/ (hooks, agents, commands). Run once after cloning or whenever claude-config/ changes.
# Root is this script's repo (scripts/swarm/..), not `git rev-parse`, so it never writes into an enclosing repo.
$root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
New-Item -ItemType Directory -Force "$root\.claude" | Out-Null
Copy-Item -Recurse -Force "$root\claude-config\*" "$root\.claude\"
Write-Host "installed .claude/ from claude-config/:"; Get-ChildItem -Recurse "$root\.claude" -File | Where-Object { $_.FullName -notmatch '\\state\\' } | ForEach-Object { "  " + $_.FullName.Substring($root.Length + 1) }
