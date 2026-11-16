#!/usr/bin/env bash
set -euo pipefail

tool="${1:-}"
case "$tool" in skill-router|turn-guard|precompact-handoff|resume-hook|syntax-guard-hook|telemetry-hook) ;; *) exit 0 ;; esac
# Only the Codex plugin dispatches here; Codex SessionStart carries no turn_id.
export OMS_HOOK_AGENT="${OMS_HOOK_AGENT:-codex}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_ROOT="$(cd "${CLAUDE_PLUGIN_ROOT:-$SCRIPT_DIR/..}" 2>/dev/null && pwd || printf '%s\n' "$SCRIPT_DIR/..")"
SOURCE_ROOT=""
if [ -f "$PLUGIN_ROOT/.oh-my-setting-source-root" ]; then
  IFS= read -r SOURCE_ROOT < "$PLUGIN_ROOT/.oh-my-setting-source-root" || SOURCE_ROOT=""
fi

# Native-first before the payload is read: the first checkout that carries
# this hook owns the command (scripts/lib/platform.sh). A harness child's
# skill-router/turn-guard stays a no-op.
case "${OMS_NATIVE_DISPATCHED:-}:$tool:${OMS_HARNESS_CHILD:-0}" in
  1:*|*:skill-router:1|*:turn-guard:1) exit 0 ;;
esac

for root in "$SOURCE_ROOT" "${OH_MY_SETTING_DIR:-}" "$PLUGIN_ROOT/../.." "$HOME/.oh-my-setting"; do
  [ -n "$root" ] && [ -f "$root/scripts/$tool.sh" ] && [ -f "$root/scripts/lib/platform.sh" ] || continue
  # shellcheck source=scripts/lib/platform.sh
  . "$root/scripts/lib/platform.sh"
  oms_native_route hook "$root" harness-hook "$@" || exit 0
  exit 0
done

if command -v oms >/dev/null 2>&1; then
  exec oms harness-hook "$@" || true
fi
exit 0
