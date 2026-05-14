#!/usr/bin/env bash
set -euo pipefail

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
copy_if_exists "$HOME/.codex/skills" "codex-skills"
copy_if_exists "$HOME/.agents/skills" "agents-skills"

echo "backup: $DEST"

