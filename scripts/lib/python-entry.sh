#!/usr/bin/env bash
set -euo pipefail

# The syntax hook owns its own shell precheck and runtime activation.
case "${2:-}" in
  */syntax-guard-hook.sh)
    payload="$(cat 2>/dev/null || true)"
    if [ -n "$payload" ]; then
      printf '%s' "$payload" | "$@"
      exit $?
    fi
    ;;
  */panel-direct-work-hook.sh|*/git-move-guard-hook.sh)
    [ -n "${OMS_PANEL_MAIN_ATTEMPT:-}" ] || exit 0
    [ "${OMS_HARNESS_CHILD:-}" != 1 ] || exit 0
    case "${OMS_PANEL_SESSION:-}" in oms-*) ;; *) exit 0 ;; esac
    ;;
esac
case "${2:-}:${OMS_HARNESS_CHILD:-0}" in
  */skill-router.sh:1|*/turn-guard.sh:1) exit 0 ;;
esac

# Provider apps need the same interpreter selection as terminal entrypoints.
ROOT="$(cd "${BASH_SOURCE[0]%/*}/../.." && pwd -P)"
# shellcheck source=scripts/lib/python-runtime.sh
. "$ROOT/scripts/lib/python-runtime.sh"
if ! oms_python_runtime_require; then
  case "${2:-}" in */git-move-guard-hook.sh) exit 2 ;; esac
  exit 1
fi
exec "$@"
