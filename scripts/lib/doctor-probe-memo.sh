# shellcheck shell=bash
# Sourced by doctor descendants. The doctor owns and exports the cache directory.

oms_doctor_probe() { # SECONDS (0 = unbounded) split|merged COMMAND...
  local helper
  helper="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/doctor-probe-memo.py"
  python3 "$helper" "$@"
}
