# shellcheck shell=bash
# Sourced by doctor descendants. The doctor owns and exports the cache directory.

case "${BASH_SOURCE[0]}" in
  */*) OMS_DOCTOR_PROBE_HELPER="$(cd "${BASH_SOURCE[0]%/*}" && pwd -P)/doctor-probe-memo.py" ;;
  *) OMS_DOCTOR_PROBE_HELPER="$(pwd -P)/doctor-probe-memo.py" ;;
esac

oms_doctor_probe() { # SECONDS (0 = unbounded) split|merged COMMAND...
  # Without a run cache the shell runs the probe itself: Python's subprocess
  # resolves commands differently on Windows (PATHEXT .cmd shims, C:\ paths).
  # Callers that pass a bound only arrive here with a cache.
  if [ -z "${OMS_DOCTOR_PROBE_DIR:-}" ]; then
    if [ "$2" = merged ]; then
      shift 2
      "$@" 2>&1
    else
      shift 2
      "$@"
    fi
    return
  fi
  python3 "$OMS_DOCTOR_PROBE_HELPER" "$@"
}
