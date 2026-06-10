#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

load_user_tool_paths() {
  export PATH="$HOME/.local/bin:$PATH"
  export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"

  if [ -s "$NVM_DIR/nvm.sh" ]; then
    # shellcheck disable=SC1091
    . "$NVM_DIR/nvm.sh"
    nvm use default >/dev/null 2>&1 || true
  fi
}

tool_version() {
  local name="$1"
  local version=""
  if command -v timeout >/dev/null 2>&1; then
    version="$(timeout 10 "$name" --version 2>/dev/null | head -n 1 || true)"
  else
    version="$("$name" --version 2>/dev/null | head -n 1 || true)"
  fi
  printf '%s' "$version" | cut -c1-64
}

tool_status() {
  local name="$1"
  local version
  if command -v "$name" >/dev/null 2>&1; then
    version="$(tool_version "$name")"
    if [ -n "$version" ]; then
      printf -- '- %s: %s (%s)\n' "$name" "$(command -v "$name")" "$version"
    else
      printf -- '- %s: %s\n' "$name" "$(command -v "$name")"
    fi
  else
    printf -- '- %s: missing\n' "$name"
  fi
}

link_status() {
  local target="$1"
  local expected="$2"
  local current

  if [ -L "$target" ]; then
    current="$(readlink "$target")"
    if [ "$current" = "$expected" ]; then
      printf -- '- %s: linked\n' "$target"
    else
      printf -- '- %s: linked elsewhere -> %s\n' "$target" "$current"
    fi
  elif [ -e "$target" ]; then
    printf -- '- %s: regular file\n' "$target"
  else
    printf -- '- %s: missing\n' "$target"
  fi
}

file_status() {
  local path="$1"
  if [ -e "$path" ] || [ -L "$path" ]; then
    if modified="$(stat -c '%y' "$path" 2>/dev/null)"; then
      printf -- '- %s: present, updated %s\n' "$path" "${modified%%.*}"
    else
      printf -- '- %s: present\n' "$path"
    fi
  else
    printf -- '- %s: missing\n' "$path"
  fi
}

load_user_tool_paths

printf '# oh-my-setting status\n\n'
printf -- '- root: %s\n' "$ROOT"
if [ -f "$ROOT/VERSION" ]; then
  printf -- '- version: %s\n' "$(head -n 1 "$ROOT/VERSION")"
fi
if git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  printf -- '- branch: %s\n' "$(git -C "$ROOT" rev-parse --abbrev-ref HEAD)"
  printf -- '- commit: %s\n' "$(git -C "$ROOT" rev-parse --short HEAD)"
fi

printf '\n## Agent config links\n\n'
link_status "$HOME/.codex/AGENTS.md" "$ROOT/AGENTS.md"
link_status "$HOME/.claude/CLAUDE.md" "$ROOT/AGENTS.md"
link_status "$HOME/.gemini/AGENTS.md" "$ROOT/AGENTS.md"
link_status "${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}/AGENTS.md" "$ROOT/AGENTS.md"

printf '\n## Required tools\n\n'
for tool in git curl node npm uv claude codex agy gh; do
  tool_status "$tool"
done

printf '\n## Optional tools\n\n'
for tool in pi timeout sbatch srun squeue sinfo scancel; do
  tool_status "$tool"
done

printf '\n## Snapshots\n\n'
file_status "$ROOT/local/machine.md"
file_status "$ROOT/custom-skills/slurm-hpc/references/cluster.generated.md"
