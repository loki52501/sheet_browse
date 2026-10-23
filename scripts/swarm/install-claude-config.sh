#!/usr/bin/env bash
# Root is this script's repo (scripts/swarm/..), not `git rev-parse`, so it never writes into an enclosing repo.
root="$(cd "$(dirname "$0")/../.." && pwd)"; mkdir -p "$root/.claude"; cp -r "$root/claude-config/." "$root/.claude/"; find "$root/.claude" -type f -not -path '*/state/*'
