#!/usr/bin/env bash

# Feedback only. Other sessions avoid the interpreter and failures stay silent.
[ -n "${OMS_PANEL_MAIN_ATTEMPT:-}" ] || exit 0
[ "${OMS_HARNESS_CHILD:-}" != 1 ] || exit 0
case "${OMS_PANEL_SESSION:-}" in oms-*) ;; *) exit 0 ;; esac
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)" || exit 0
python3 -B "$ROOT/scripts/lib/hook_state.py" panel-direct-work 2>/dev/null || true
exit 0
