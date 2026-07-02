#!/usr/bin/env bash
set -euo pipefail

# Snapshot the agent config files oh-my-setting replaces into backups/<timestamp>.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STAMP="$(date +%Y%m%d%H%M%S)"
DEST="$ROOT/backups/$STAMP"

mkdir -p "$DEST"

copy_if_exists() {
  local source="$1"
  local name="$2"

  if [ -e "$source" ] || [ -L "$source" ]; then
    cp -a "$source" "$DEST/$name"
    echo "backed up $source -> $DEST/$name"
  fi
}

copy_if_exists "$HOME/.codex/AGENTS.md" "codex-AGENTS.md"
copy_if_exists "$HOME/.claude/CLAUDE.md" "claude-CLAUDE.md"
copy_if_exists "$HOME/.gemini/AGENTS.md" "gemini-AGENTS.md"
copy_if_exists "$HOME/.codex/skills" "codex-skills"
copy_if_exists "$HOME/.claude/skills" "claude-skills"
copy_if_exists "$HOME/.gemini/antigravity/skills" "gemini-skills"

echo "backup: $DEST"
