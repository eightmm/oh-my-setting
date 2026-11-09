#!/usr/bin/env bash
# Registered hook commands start with a bare `bash`, which a GUI/hook PATH
# resolves to the system Bash 3.2 on macOS. Re-enter under the interpreter
# install recorded (the first line of bash-interpreter beside the receipt) and
# run the hook with it too; an unusable record fails closed with one line.
if [ "${BASH_VERSINFO[0]:-0}" -lt 5 ]; then
  oms_rec_file="${OMS_INSTALL_RECEIPT:-${XDG_CONFIG_HOME:-$HOME/.config}/oh-my-setting/install.json}"
  oms_rec="$(sed -n '1p' "${oms_rec_file%/*}/bash-interpreter" 2>/dev/null || true)"
  case "$oms_rec" in
    /*)
      if [ -z "${OMS_BASH_REEXEC:-}" ] && [ -f "$oms_rec" ] && [ -x "$oms_rec" ] &&
         "$oms_rec" -c '[ "${BASH_VERSINFO[0]}" -ge 5 ]' >/dev/null 2>&1; then
        OMS_BASH_REEXEC="$oms_rec" exec "$oms_rec" "$0" "$@"
      fi
      ;;
  esac
  echo "error: oh-my-setting needs Bash 5 or newer (running ${BASH_VERSION:-unknown}); on macOS run: brew install bash, then rerun" >&2
  case "${2:-}" in */git-move-guard-hook.sh) exit 2 ;; esac
  exit 1
fi
set -euo pipefail
if [ -n "${OMS_BASH_REEXEC:-}" ] && [ "${1:-}" = bash ]; then
  shift
  set -- "$OMS_BASH_REEXEC" "$@"
fi
unset OMS_BASH_REEXEC
# Hook helpers start through `#!/usr/bin/env bash`: make them see this Bash.
# shellcheck source=scripts/lib/platform.sh
. "${BASH_SOURCE[0]%/*}/platform.sh"
oms_bash_enter

# The syntax hook owns its own shell precheck and runtime activation.
case "${2:-}" in
  */syntax-guard-hook.sh)
    payload="$(cat 2>/dev/null || true)"
    if [ -n "$payload" ]; then
      printf '%s' "$payload" | "$@"
      exit $?
    fi
    ;;
  */git-move-guard-hook.sh)
    [ -n "${OMS_PANEL_MAIN_ATTEMPT:-}" ] || exit 0
    [ "${OMS_HARNESS_CHILD:-}" != 1 ] || exit 0
    case "${OMS_PANEL_SESSION:-}" in oms-*) ;; *) exit 0 ;; esac
    ;;
  */command-guard-hook.sh)
    [ "${OMS_COMMAND_GUARD:-1}" != 0 ] || exit 0
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
