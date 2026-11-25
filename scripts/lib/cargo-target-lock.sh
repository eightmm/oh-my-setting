# shellcheck shell=bash
# A verification run builds test executables into one Cargo output directory
# and then runs them. A second run on the same physical directory would replace
# those executables mid-run (#508), so runs sharing it take turns. The holder
# keeps the lock until it exits; its children inherit it via
# OMS_CARGO_TARGET_HELD and do not wait on their parent.
# shellcheck source=scripts/lib/file-lock.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/file-lock.sh"

OMS_CARGO_TARGET_LOCK=""
OMS_CARGO_TARGET_OWNER=""

oms_cargo_target_hold() {  # ROOT LABEL
  local dir="${CARGO_TARGET_DIR:-${CARGO_BUILD_TARGET_DIR:-$1/target}}" label="$2"
  local wait="${OMS_CARGO_TARGET_WAIT:-3600}" lock holder="" start rc=0
  case "$wait" in ''|*[!0-9]*) wait=3600 ;; esac
  mkdir -p "$dir" && dir="$(cd "$dir" && pwd -P)" || return 1
  [ "${OMS_CARGO_TARGET_HELD:-}" != "$dir" ] || return 0
  # Keyed by the physical directory under the cache lock dir, never the temp
  # root: two runs with different TMPDIR values must still meet.
  lock="$(oms_file_lock_dir)/cargo-target.$(printf '%s' "$dir" | cksum | awk '{print $1 "-" $2}').lock"
  mkdir -p "${lock%/*}" || return 1
  oms_file_lock_set_current_identity || return $?
  OMS_CARGO_TARGET_OWNER="$OMS_FILE_LOCK_CURRENT_PID.$(date +%s).${RANDOM:-0}"
  holder="$(sed -n '1p' "$lock/pid" 2>/dev/null || true)"
  if [ -z "$holder" ] || oms_file_lock_mkdir_stale "$lock" "$wait" "$(date +%s)"; then
    holder=""
  fi
  [ -z "$holder" ] ||
    echo "$label: waiting up to ${wait}s for Cargo target $dir, in use by verification pid $holder" >&2
  start="$(date +%s)"
  oms_file_lock_mkdir_acquire "$dir" "$lock" "$wait" "$OMS_CARGO_TARGET_OWNER" || rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "$label: Cargo target $dir is still in use; rerun after that verification ends or set CARGO_TARGET_DIR to a separate directory" >&2
    return "$rc"
  fi
  [ -z "$holder" ] || echo "$label: Cargo target $dir acquired after $(($(date +%s) - start))s" >&2
  OMS_CARGO_TARGET_LOCK="$lock"
  export OMS_CARGO_TARGET_HELD="$dir"
}

oms_cargo_target_release() {
  [ -n "$OMS_CARGO_TARGET_LOCK" ] || return 0
  oms_file_lock_mkdir_release "$OMS_CARGO_TARGET_LOCK" "$OMS_CARGO_TARGET_OWNER"
  OMS_CARGO_TARGET_LOCK=""
}
