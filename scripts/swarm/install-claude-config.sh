#!/usr/bin/env bash
root="$(git rev-parse --show-toplevel)"; mkdir -p "$root/.claude"; cp -r "$root/claude-config/." "$root/.claude/"; find "$root/.claude" -type f
