#!/usr/bin/env bash
set -euo pipefail

tool="${1:-}"
case "$tool" in skill-router|turn-guard|precompact-handoff|resume-hook|syntax-guard-hook|telemetry-hook) ;; *) exit 0 ;; esac
payload="$(cat)"
# Only the Codex plugin dispatches here; Codex SessionStart carries no turn_id.
export OMS_HOOK_AGENT="${OMS_HOOK_AGENT:-codex}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_ROOT="$(cd "${CLAUDE_PLUGIN_ROOT:-$SCRIPT_DIR/..}" 2>/dev/null && pwd || printf '%s\n' "$SCRIPT_DIR/..")"
SOURCE_ROOT=""
if [ -f "$PLUGIN_ROOT/.oh-my-setting-source-root" ]; then
  IFS= read -r SOURCE_ROOT < "$PLUGIN_ROOT/.oh-my-setting-source-root" || SOURCE_ROOT=""
fi

# Under Bash 3.2 the launcher re-execs the recorded Bash 5; under Bash 5 keep it.
launcher_bash=bash
case "${BASH:-}" in /*) [ "${BASH_VERSINFO[0]:-0}" -lt 5 ] || launcher_bash="$BASH" ;; esac

emit_hook_output() {
  if [ "$tool" = "resume-hook" ]; then
    # The resume block begins with a bracket, which Codex treats as JSON-like;
    # Claude's direct hook still receives the shared script's plain-text context.
    python3 -X utf8 -I -B -c '
import json, sys

context = sys.stdin.read()
if context:
    sys.stdout.write(json.dumps({"hookSpecificOutput": {
        "hookEventName": "SessionStart",
        "additionalContext": context,
    }}) + "\n")
'
  else
    cat
  fi
}

for root in "$SOURCE_ROOT" "${OH_MY_SETTING_DIR:-}" "$PLUGIN_ROOT/../.." "$HOME/.oh-my-setting"; do
  [ -n "$root" ] || continue
  if [ -f "$root/scripts/$tool.sh" ]; then
    if [ "$tool" = syntax-guard-hook ]; then
      printf '%s' "$payload" | "$launcher_bash" "$root/scripts/lib/python-entry.sh" bash "$root/scripts/$tool.sh" "${@:2}" || true
      exit 0
    fi
    case "$tool:${OMS_HARNESS_CHILD:-0}" in skill-router:1|turn-guard:1) exit 0 ;; esac
    # Select once in the parent: resume output formatting also runs Python.
    # shellcheck source=scripts/lib/python-runtime.sh
    . "$root/scripts/lib/python-runtime.sh"
    ROOT="$root"
    if ! oms_python_runtime_require; then
      [ "$tool" != turn-guard ] || exit 1
      exit 0
    fi
    printf '%s' "$payload" | "$launcher_bash" "$root/scripts/lib/python-entry.sh" bash "$root/scripts/$tool.sh" "${@:2}" | emit_hook_output || true
    exit 0
  fi
done

if command -v oms >/dev/null 2>&1; then
  printf '%s' "$payload" | oms "$tool" | emit_hook_output || true
fi
exit 0
