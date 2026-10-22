# shellcheck shell=bash
# Canonical snapshots and checkout refreshes share one admission gate. A call
# keeps its own entry after leaving the gate; sync holds the gate until Git ends.

# shellcheck source-path=SCRIPTDIR
# shellcheck source=file-lock.sh
. "$(cd "${BASH_SOURCE[0]%/*}" && pwd)/file-lock.sh"

oms_canonical_sync_prepare() {  # REPO
  local dir
  for dir in "$1/.oms" "$1/.oms/locks" "$1/.oms/locks/canonical-sync.d"; do
    [ ! -L "$dir" ] || { echo "error: symbolic canonical lock directory: $dir" >&2; return 2; }
    mkdir -p "$dir" || return 2
  done
}

oms_canonical_sync_gate() {  # REPO COMMAND ARGS...
  local repo="$1"
  shift
  oms_with_file_lock_mkdir "$repo/.oms/locks/canonical-sync" \
    "$repo/.oms/locks/canonical-sync.gate" 5 "$@"
}

oms_canonical_call_register() {  # ENTRY OWNER PID START
  local entry="$1" owner="$2" pid="$3" start="$4" pending
  [ ! -e "$entry" ] && [ ! -L "$entry" ] || return 2
  # Stage outside the scanned registry. A crash before publication leaves no
  # live entry; after rename the complete PID-generation identity is visible.
  pending="$(mktemp -d "${entry%/*}/../.canonical-call.XXXXXX")" || return 2
  if ! { printf '%s\n' "$owner" > "$pending/owner" &&
    printf '%s\n' "$pid" > "$pending/pid" &&
    printf '%s\n' "$start" > "$pending/process-start" &&
    mv "$pending" "$entry"; }; then
    rm -rf -- "$pending"
    return 2
  fi
}

OMS_CANONICAL_CALL_ENTRY=""
OMS_CANONICAL_CALL_OWNER=""
oms_canonical_call_acquire() {  # REPO CALL_ID
  local repo="$1" id="$2" start
  case "$id" in
    ""|*[!A-Za-z0-9._-]*|.|..) echo 'error: invalid canonical call id' >&2; return 2 ;;
  esac
  oms_canonical_sync_prepare "$repo" || return $?
  start="$(oms_file_lock_process_start_token "$$")" || return 2
  start="${start//$'\r'/}"
  [ -n "$start" ] || return 2
  OMS_CANONICAL_CALL_ENTRY="$repo/.oms/locks/canonical-sync.d/$id"
  OMS_CANONICAL_CALL_OWNER="$$.$start.${RANDOM:-0}"
  oms_canonical_sync_gate "$repo" oms_canonical_call_register \
    "$OMS_CANONICAL_CALL_ENTRY" "$OMS_CANONICAL_CALL_OWNER" "$$" "$start"
}

oms_canonical_call_release() {
  [ -n "$OMS_CANONICAL_CALL_ENTRY" ] || return 0
  oms_file_lock_mkdir_release "$OMS_CANONICAL_CALL_ENTRY" "$OMS_CANONICAL_CALL_OWNER"
  OMS_CANONICAL_CALL_ENTRY=""
}

oms_canonical_sync_live_entries() {  # REPO (read-only; malformed entries stay live)
  local repo="$1" entry pid saved current
  for entry in "$repo/.oms/locks/canonical-sync.d/"*; do
    [ -e "$entry" ] || [ -L "$entry" ] || continue
    if [ -d "$entry" ] && [ ! -L "$entry" ] &&
      [ -f "$entry/pid" ] && [ ! -L "$entry/pid" ] && [ -r "$entry/pid" ] &&
      [ -f "$entry/process-start" ] && [ ! -L "$entry/process-start" ] && [ -r "$entry/process-start" ]; then
      pid="" saved="" current=""
      IFS= read -r pid < "$entry/pid" || true
      IFS= read -r saved < "$entry/process-start" || true
      pid="${pid//$'\r'/}" saved="${saved//$'\r'/}"
      case "$pid" in
        ""|0|*[!0-9]*) ;;
        *)
          if [ -n "$saved" ]; then
            current="$(oms_file_lock_process_start_token "$pid" 2>/dev/null || true)"
            current="${current//$'\r'/}"
            if ! oms_file_lock_pid_alive "$pid" ||
              { [ -n "$current" ] && [ "$current" != "$saved" ]; }; then
              continue
            fi
          fi
          ;;
      esac
    fi
    printf '%s\n' "${entry##*/}"
  done
}

oms_canonical_sync_live_review_entries() {  # REPO (uncertain owners stay live)
  local repo="$1" name entry pid command
  while IFS= read -r name; do
    [ -n "$name" ] || continue
    case "$name" in review-*) printf '%s\n' "$name"; continue ;; esac
    entry="$repo/.oms/locks/canonical-sync.d/$name"
    if [ ! -d "$entry" ] || [ -L "$entry" ] ||
      [ ! -f "$entry/pid" ] || [ -L "$entry/pid" ] || [ ! -r "$entry/pid" ]; then
      printf '%s\n' "$name"
      continue
    fi
    pid=""
    IFS= read -r pid < "$entry/pid" || true
    pid="${pid//$'\r'/}"
    case "$pid" in
      ""|0|*[!0-9]*) printf '%s\n' "$name"; continue ;;
    esac
    command="$(ps -p "$pid" -o command= 2>/dev/null | tr -d '\r' || true)"
    case "$command" in
      *peer-review.sh*|"") printf '%s\n' "$name" ;;
    esac
  done <<EOF
$(oms_canonical_sync_live_entries "$repo")
EOF
}

oms_canonical_sync_if_idle() {  # REPO COMMAND ARGS... (under gate)
  local repo="$1" entry pid saved current live="" count=0
  shift
  for entry in "$repo/.oms/locks/canonical-sync.d/"*; do
    [ -e "$entry" ] || [ -L "$entry" ] || continue
    pid="" saved="" current=""
    if [ -d "$entry" ] && [ ! -L "$entry" ] &&
        [ -f "$entry/pid" ] && [ ! -L "$entry/pid" ] &&
        [ -f "$entry/process-start" ] && [ ! -L "$entry/process-start" ]; then
      IFS= read -r pid < "$entry/pid" || true
      IFS= read -r saved < "$entry/process-start" || true
      pid="${pid//$'\r'/}" saved="${saved//$'\r'/}"
      case "$pid" in
        ""|0|*[!0-9]*) ;;
        *)
          if [ -n "$saved" ]; then
            current="$(oms_file_lock_process_start_token "$pid" 2>/dev/null || true)"
            current="${current//$'\r'/}"
            if ! oms_file_lock_pid_alive "$pid" ||
                { [ -n "$current" ] && [ "$current" != "$saved" ]; }; then
              rm -rf -- "$entry" || return 2
              continue
            fi
          fi
          ;;
      esac
    fi
    live="${live:+$live, }${entry##*/}"
    count=$((count + 1))
  done
  if [ "$count" -gt 0 ]; then
    echo "deferred: $count calls running ($live)" >&2
    return 75
  fi
  "$@"
}
