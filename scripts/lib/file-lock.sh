# shellcheck shell=bash
# Shared per-file inter-process locks. Sourced, not executed.

oms_file_lock_timeout() {
  local timeout="${OMS_LOCK_TIMEOUT:-300}"

  case "$timeout" in
    *[!0-9]*|"") timeout=300 ;;
  esac
  [ "$timeout" -gt 0 ] || timeout=300
  printf '%s\n' "$timeout"
}

# Locks live in a per-user dir, not next to the state file: a sibling
# "<file>.lock" litters git-tracked dirs (e.g. docs/EXPERIMENTS.jsonl), and a
# released flock file cannot be unlinked safely (unlink lets two holders
# coexist on different inodes). Keyed by absolute path via cksum.
# Deliberately NOT under XDG_RUNTIME_DIR: an interactive session (set) and a
# cron/ssh session (unset) would compute different lock dirs for the same
# state file, and both could enter the same critical section.
oms_file_lock_dir() {
  printf '%s\n' "$HOME/.cache/oh-my-setting/locks"
}

oms_file_lock_path_for_file() {
  local file="$1"
  local abs
  local name
  local sum

  case "$file" in
    /*) abs="$file" ;;
    *) abs="$PWD/$file" ;;
  esac
  name="$(printf '%s' "$(basename "$abs")" | tr -c 'A-Za-z0-9._-' '_')"
  sum="$(printf '%s' "$abs" | cksum | awk '{print $1 "-" $2}')"
  printf '%s/%s.%s.lock\n' "$(oms_file_lock_dir)" "$name" "$sum"
}

oms_file_lock_pid_alive() {
  local pid="$1"

  case "$pid" in
    *[!0-9]*|"") return 1 ;;
  esac
  kill -0 "$pid" 2>/dev/null
}

oms_file_lock_mkdir_stale() {
  local lock_dir="$1"
  local timeout="$2"
  local now="$3"
  local pid=""
  local started=""

  [ -d "$lock_dir" ] || return 1
  [ -f "$lock_dir/pid" ] && pid="$(sed -n '1p' "$lock_dir/pid" 2>/dev/null || true)"
  if [ -n "$pid" ] && ! oms_file_lock_pid_alive "$pid"; then
    return 0
  fi

  [ -f "$lock_dir/started" ] && started="$(sed -n '1p' "$lock_dir/started" 2>/dev/null || true)"
  case "$started" in
    *[!0-9]*|"") return 1 ;;
  esac
  [ $((now - started)) -ge "$timeout" ]
}

oms_file_lock_mkdir_release() {
  local lock_dir="$1"
  local owner_id="$2"
  local owner=""

  [ -d "$lock_dir" ] || return 0
  [ -f "$lock_dir/owner" ] && owner="$(sed -n '1p' "$lock_dir/owner" 2>/dev/null || true)"
  [ "$owner" = "$owner_id" ] || return 0
  rm -rf "$lock_dir"
}

oms_file_lock_mkdir_acquire() {
  local state_file="$1"
  local lock_dir="$2"
  local timeout="$3"
  local owner_id="$4"
  local start
  local now
  local stale_dir

  start="$(date +%s)"
  while :; do
    if mkdir "$lock_dir" 2>/dev/null; then
      printf '%s\n' "$$" > "$lock_dir/pid"
      printf '%s\n' "$(date +%s)" > "$lock_dir/started"
      printf '%s\n' "$owner_id" > "$lock_dir/owner"
      return 0
    fi

    now="$(date +%s)"
    if oms_file_lock_mkdir_stale "$lock_dir" "$timeout" "$now"; then
      stale_dir="$lock_dir.stale.$$"
      if mv "$lock_dir" "$stale_dir" 2>/dev/null; then
        rm -rf "$stale_dir"
        continue
      fi
    fi

    if [ $((now - start)) -ge "$timeout" ]; then
      echo "error: could not acquire lock for $state_file after ${timeout}s: $lock_dir" >&2
      return 75
    fi
    sleep 1
  done
}

oms_with_file_lock_mkdir() {
  local state_file="$1"
  local lock_dir="$2"
  local timeout="$3"
  local owner_id
  shift 3

  owner_id="$$.$(date +%s).${RANDOM:-0}"
  (
    oms_file_lock_mkdir_acquire "$state_file" "$lock_dir" "$timeout" "$owner_id" || exit $?
    lock_cleanup_done=0
    oms_file_lock_cleanup() {
      [ "$lock_cleanup_done" = 0 ] || return 0
      lock_cleanup_done=1
      oms_file_lock_mkdir_release "$lock_dir" "$owner_id"
    }
    oms_file_lock_cleanup_signal() {
      local code="$1"
      trap - EXIT HUP INT TERM
      oms_file_lock_cleanup
      exit "$code"
    }
    trap oms_file_lock_cleanup EXIT
    trap 'oms_file_lock_cleanup_signal 129' HUP
    trap 'oms_file_lock_cleanup_signal 130' INT
    trap 'oms_file_lock_cleanup_signal 143' TERM
    "$@"
  )
}

oms_try_file_lock_mkdir_acquire() {
  local lock_dir="$1"
  local timeout="$2"
  local owner_id="$3"
  local now
  local stale_dir

  while :; do
    if mkdir "$lock_dir" 2>/dev/null; then
      printf '%s\n' "$$" > "$lock_dir/pid"
      printf '%s\n' "$(date +%s)" > "$lock_dir/started"
      printf '%s\n' "$owner_id" > "$lock_dir/owner"
      return 0
    fi

    now="$(date +%s)"
    if oms_file_lock_mkdir_stale "$lock_dir" "$timeout" "$now"; then
      stale_dir="$lock_dir.stale.$$"
      if mv "$lock_dir" "$stale_dir" 2>/dev/null; then
        rm -rf "$stale_dir"
        continue
      fi
    fi

    return 75
  done
}

oms_try_file_lock_mkdir() {
  local lock_dir="$2"
  local timeout="$3"
  local owner_id
  shift 3

  owner_id="$$.$(date +%s).${RANDOM:-0}"
  (
    oms_try_file_lock_mkdir_acquire "$lock_dir" "$timeout" "$owner_id" || exit $?
    lock_cleanup_done=0
    oms_file_lock_cleanup() {
      [ "$lock_cleanup_done" = 0 ] || return 0
      lock_cleanup_done=1
      oms_file_lock_mkdir_release "$lock_dir" "$owner_id"
    }
    oms_file_lock_cleanup_signal() {
      local code="$1"
      trap - EXIT HUP INT TERM
      oms_file_lock_cleanup
      exit "$code"
    }
    trap oms_file_lock_cleanup EXIT
    trap 'oms_file_lock_cleanup_signal 129' HUP
    trap 'oms_file_lock_cleanup_signal 130' INT
    trap 'oms_file_lock_cleanup_signal 143' TERM
    "$@"
  )
}

oms_try_file_lock() {
  local state_file="$1"
  local timeout
  local lock_path
  local lock_parent
  shift

  [ -n "$state_file" ] || {
    echo "error: lock target is required" >&2
    return 2
  }
  [ "$#" -gt 0 ] || {
    echo "error: lock command is required for $state_file" >&2
    return 2
  }

  timeout="$(oms_file_lock_timeout)"
  lock_path="$(oms_file_lock_path_for_file "$state_file")"
  lock_parent="$(dirname "$lock_path")"
  mkdir -p "$lock_parent"

  if command -v flock >/dev/null 2>&1 && [ "${OMS_LOCK_FORCE_MKDIR:-0}" != "1" ]; then
    (
      exec 9>"$lock_path" || exit 75
      flock -n 9 || exit 75
      "$@"
    )
  else
    oms_try_file_lock_mkdir "$state_file" "$lock_path" "$timeout" "$@"
  fi
}

oms_with_file_lock() {
  local state_file="$1"
  local timeout
  local lock_path
  local lock_parent
  local start
  local now
  shift

  [ -n "$state_file" ] || {
    echo "error: lock target is required" >&2
    return 2
  }
  [ "$#" -gt 0 ] || {
    echo "error: lock command is required for $state_file" >&2
    return 2
  }

  timeout="$(oms_file_lock_timeout)"
  lock_path="$(oms_file_lock_path_for_file "$state_file")"
  lock_parent="$(dirname "$lock_path")"
  mkdir -p "$lock_parent"

  if command -v flock >/dev/null 2>&1 && [ "${OMS_LOCK_FORCE_MKDIR:-0}" != "1" ]; then
    (
      exec 9>"$lock_path" || {
        echo "error: could not open lock for $state_file: $lock_path" >&2
        exit 75
      }
      start="$(date +%s)"
      while ! flock -n 9; do
        now="$(date +%s)"
        if [ $((now - start)) -ge "$timeout" ]; then
          echo "error: could not acquire lock for $state_file after ${timeout}s: $lock_path" >&2
          exit 75
        fi
        sleep 1
      done
      "$@"
    )
  else
    oms_with_file_lock_mkdir "$state_file" "$lock_path" "$timeout" "$@"
  fi
}
