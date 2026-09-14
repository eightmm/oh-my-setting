# shellcheck shell=bash

# Checks may overlap each other, but not a maintenance sweep of the same repo.
# The existing file mutex protects registration and the whole sweep. Per-check
# process markers live beside that mutex, outside the state being inventoried.
# shellcheck source=scripts/lib/file-lock.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/file-lock.sh"

oms_check_maintenance_write_marker() {
  local marker="$1" owner="$2" pid="$3" start="$4"
  mkdir -p "$(dirname "$marker")"
  mkdir "$marker"
  printf '%s\n' "$owner" > "$marker/owner"
  printf '%s\n' "$pid" > "$marker/pid"
  printf '%s\n' "$start" > "$marker/process-start"
  date +%s > "$marker/started"
}

oms_check_maintenance_begin() {
  local repo target lock_path
  repo="$(cd "$1" && pwd -P)" || return 1
  target="$repo/.oms/tick/last.json"
  lock_path="$(oms_file_lock_path_for_file "$target")"
  oms_file_lock_set_current_identity || return $?
  OMS_CHECK_MAINTENANCE_OWNER="$OMS_FILE_LOCK_CURRENT_PID.$(date +%s).${RANDOM:-0}"
  OMS_CHECK_MAINTENANCE_MARKER="$lock_path.checks/$OMS_CHECK_MAINTENANCE_OWNER"
  oms_with_file_lock "$target" oms_check_maintenance_write_marker \
    "$OMS_CHECK_MAINTENANCE_MARKER" "$OMS_CHECK_MAINTENANCE_OWNER" \
    "$OMS_FILE_LOCK_CURRENT_PID" "$OMS_FILE_LOCK_CURRENT_START"
}

oms_check_maintenance_end() {
  [ -n "${OMS_CHECK_MAINTENANCE_MARKER:-}" ] || return 0
  oms_file_lock_mkdir_release "$OMS_CHECK_MAINTENANCE_MARKER" "$OMS_CHECK_MAINTENANCE_OWNER"
}

# Call while holding the receipt mutex. Dead checks do not disable maintenance;
# uncertain/live identities remain conservative, regardless of marker age.
oms_check_maintenance_active() {
  local checks marker owner
  checks="$(oms_file_lock_path_for_file "$1/.oms/tick/last.json").checks"
  for marker in "$checks"/*; do
    [ -d "$marker" ] || continue
    if oms_file_lock_mkdir_stale "$marker" "$(oms_file_lock_timeout)" "$(date +%s)"; then
      owner="$(sed -n '1p' "$marker/owner" 2>/dev/null || true)"
      [ -z "$owner" ] || oms_file_lock_mkdir_release "$marker" "$owner"
    else
      return 0
    fi
  done
  return 1
}
