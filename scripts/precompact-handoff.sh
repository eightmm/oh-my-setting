#!/usr/bin/env bash
[ "${BASH_VERSINFO[0]:-0}" -ge 5 ] || exit 0
set -euo pipefail

# Native OMS entrypoint for precompact-handoff.

case "${OMS_PLATFORM_OVERRIDE:-$(uname -s)}:$(uname -s):${OSTYPE:-}" in
  windows:*|Windows:*|WINDOWS:*|MINGW*|MSYS*|CYGWIN*|*:MINGW*|*:MSYS*|*:CYGWIN*|*:msys*|*:cygwin*|*:win32)
    exit 0
    ;;
esac

script_dir="${BASH_SOURCE[0]%/*}"
[ "$script_dir" != "${BASH_SOURCE[0]}" ] || script_dir=.
ROOT="$(cd "$script_dir/.." && pwd -P)"
if [ "${OMS_NATIVE_DISPATCHED:-}" = 1 ] ||
   [ "${OMS_NATIVE_OUTCOME:-}" = absent ] ||
   [ "${OMS_NATIVE_OUTCOME:-}" = unverified ]; then
  exit 0
fi
# shellcheck source=scripts/lib/platform.sh
. "$ROOT/scripts/lib/platform.sh"
oms_bash_enter
oms_native_route hook "$ROOT" "precompact-handoff" "$@" || exit 0
exit 0
