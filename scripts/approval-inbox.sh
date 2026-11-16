#!/usr/bin/env bash
[ "${BASH_VERSINFO[0]:-0}" -ge 5 ] || { echo "error: oh-my-setting needs Bash 5 or newer (running ${BASH_VERSION:-unknown}); on macOS run: brew install bash, then rerun" >&2; exit 1; }
set -euo pipefail

# Native OMS entrypoint for approval-inbox.

case "${OMS_PLATFORM_OVERRIDE:-$(uname -s)}:$(uname -s):${OSTYPE:-}" in
  windows:*|Windows:*|WINDOWS:*|MINGW*|MSYS*|CYGWIN*|*:MINGW*|*:MSYS*|*:CYGWIN*|*:msys*|*:cygwin*|*:win32)
    echo 'native Windows (Git Bash/MSYS2/Cygwin) is not supported; install inside WSL2 with the repository and home on the Linux filesystem (not /mnt/c).' >&2
    exit 1
    ;;
esac

script_dir="${BASH_SOURCE[0]%/*}"
[ "$script_dir" != "${BASH_SOURCE[0]}" ] || script_dir=.
ROOT="$(cd "$script_dir/.." && pwd -P)"
if [ "${OMS_NATIVE_DISPATCHED:-}" = 1 ] ||
   [ "${OMS_NATIVE_OUTCOME:-}" = absent ] ||
   [ "${OMS_NATIVE_OUTCOME:-}" = unverified ]; then
  echo "error: native payload unavailable for approval-inbox; stage with cargo or run oms doctor --repair" >&2
  exit 2
fi
# shellcheck source=scripts/lib/platform.sh
. "$ROOT/scripts/lib/platform.sh"
oms_bash_enter
oms_native_route "strict" "$ROOT" "approval-inbox" "$@" || exit $?
echo "error: native payload unavailable for approval-inbox (${OMS_NATIVE_OUTCOME:-unknown}); stage with cargo or run oms doctor --repair" >&2
exit 2
