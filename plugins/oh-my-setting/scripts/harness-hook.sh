#!/usr/bin/env bash
set -euo pipefail

tool="${1:-}"
case "$tool" in skill-router|turn-guard) ;; *) exit 0 ;; esac
payload="$(cat)"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_ROOT="$(cd "${CLAUDE_PLUGIN_ROOT:-$SCRIPT_DIR/..}" 2>/dev/null && pwd || printf '%s\n' "$SCRIPT_DIR/..")"
SOURCE_ROOT=""
if [ -f "$PLUGIN_ROOT/.oh-my-setting-source-root" ]; then
  IFS= read -r SOURCE_ROOT < "$PLUGIN_ROOT/.oh-my-setting-source-root" || SOURCE_ROOT=""
fi

for root in "$SOURCE_ROOT" "${OH_MY_SETTING_DIR:-}" "$PLUGIN_ROOT/../.." "$HOME/.oh-my-setting"; do
  [ -n "$root" ] || continue
  if [ -f "$root/scripts/$tool.sh" ]; then
    printf '%s' "$payload" | bash "$root/scripts/$tool.sh" || true
    exit 0
  fi
done

if command -v oms >/dev/null 2>&1; then
  printf '%s' "$payload" | oms "$tool" || true
fi
exit 0
