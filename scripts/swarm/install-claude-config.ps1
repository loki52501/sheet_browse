# Copies claude-config/ into .claude/ (hooks, agents, commands). Run once after cloning or whenever claude-config/ changes.
$root = (git rev-parse --show-toplevel).Trim()
New-Item -ItemType Directory -Force "$root\.claude" | Out-Null
Copy-Item -Recurse -Force "$root\claude-config\*" "$root\.claude\"
Write-Host "installed .claude/ from claude-config/:"; Get-ChildItem -Recurse "$root\.claude" -File | ForEach-Object { "  " + $_.FullName.Substring($root.Length + 1) }
