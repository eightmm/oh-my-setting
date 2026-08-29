#!/usr/bin/env bash

_OMS_PROJECT_STATE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
_OMS_PROJECT_STATE_HELPER="$_OMS_PROJECT_STATE_ROOT/project-state.py"

# Pure PROJECT.md publication-state parser. Callers keep their own authority;
# this only gives every read/preflight surface the same typed interpretation.
oms_project_state() {  # FILE -> missing|draft|confirmed|legacy-active|invalid
  python3 "$_OMS_PROJECT_STATE_HELPER" state "$1" | tr -d '\r'
}

oms_project_state_snapshot() {  # FILE -> typed JSON snapshot
  python3 "$_OMS_PROJECT_STATE_HELPER" snapshot "$1" | tr -d '\r'
}
