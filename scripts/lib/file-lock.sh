# shellcheck shell=bash
# Shared per-file inter-process locks. Sourced, not executed.

OMS_FILE_LOCK_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ -f "$OMS_FILE_LOCK_LIB_DIR/poll.sh" ]; then
  # shellcheck source=scripts/lib/poll.sh
  . "$OMS_FILE_LOCK_LIB_DIR/poll.sh"
else
  oms_poll_sleep_labeled() { sleep 1; }
  oms_poll_sleep() { oms_poll_sleep_labeled wait "$@"; }
fi

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
# Deliberately NOT under XDG_RUNTIME_DIR or XDG_CACHE_HOME: an interactive
# session (set) and a cron/systemd-timer session (unset) would compute
# different lock dirs for the same state file, and both could enter the same
# critical section. auto-update runs from a user timer, so that asymmetry is
# not hypothetical here. OMS_LOCK_DIR is the one override, named for exactly
# this purpose, so a test run can be hermetic without the production path
# depending on an ambient variable it does not control.
oms_file_lock_dir() {
  printf '%s\n' "${OMS_LOCK_DIR:-$HOME/.cache/oh-my-setting/locks}"
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
  printf '%s/%s.%s.lock\n' "$(oms_file_lock_dir_for_path "$abs")" "$name" "$sum"
}

# State that lives under the temp directory is a fixture or a throwaway
# checkout, and its locks used to accumulate in the per-user cache forever:
# every suite run outside check.sh (which sets OMS_LOCK_DIR) left hundreds of
# empty flock files behind, fifteen thousand on one workstation. Those locks
# go to a per-user directory under the same temp root instead, which the
# system clears and which no production state ever resolves through. The
# choice keys off the state path alone (not an ambient variable), so every
# process resolves the same lock for the same file. The directory must be
# ours and not a symlink before a lock is opened through it, because the
# lock open truncates; anything else falls back to the cache directory.
oms_file_lock_dir_for_path() {  # ABSOLUTE_STATE_PATH
  local abs="$1"
  local tmp_root="${TMPDIR:-/tmp}"
  local candidate

  [ -z "${OMS_LOCK_DIR:-}" ] || { oms_file_lock_dir; return 0; }
  tmp_root="${tmp_root%/}"
  case "$abs" in
    "$tmp_root"/*) ;;
    *) oms_file_lock_dir; return 0 ;;
  esac
  candidate="$tmp_root/oh-my-setting-locks-${UID:-$(id -u)}"
  if [ ! -e "$candidate" ]; then
    mkdir -m 700 "$candidate" 2>/dev/null || true
  fi
  if [ -d "$candidate" ] && [ ! -L "$candidate" ] && [ -O "$candidate" ]; then
    printf '%s\n' "$candidate"
  else
    oms_file_lock_dir
  fi
}

oms_file_lock_pid_alive() {
  local pid="$1"

  case "$pid" in
    *[!0-9]*|"") return 1 ;;
  esac
  kill -0 "$pid" 2>/dev/null
}

# Return the native process id corresponding to one shell-process id. Git
# Bash/MSYS exposes $$ in its own pid namespace, while Win32 OpenProcess needs
# the backing Windows pid. Native children are not an identity bridge here:
# MSYS may launch them through a short-lived process, so os.getppid() can name
# a parent that is already gone before a marker is read.
oms_process_native_pid() {  # LOGICAL_PID
  local logical_pid="${1:-}"
  local native_pid=""

  case "$logical_pid" in
    *[!0-9]*|"") return 75 ;;
  esac
  [ "$logical_pid" -gt 0 ] || return 75

  case "$(uname -s 2>/dev/null || true)" in
    MINGW*|MSYS*|CYGWIN*)
      [ -r "/proc/$logical_pid/winpid" ] || return 75
      IFS= read -r native_pid < "/proc/$logical_pid/winpid" || return 75
      native_pid="${native_pid//$'\r'/}"
      ;;
    *) native_pid="$logical_pid" ;;
  esac
  case "$native_pid" in
    *[!0-9]*|"") return 75 ;;
  esac
  [ "$native_pid" -gt 0 ] || return 75
  [ "$native_pid" -le 4294967295 ] || return 75
  printf '%s\n' "$native_pid"
}

# Persist how the native pid was derived. Readers require this exact source on
# native Windows before a stored pid may prove that an owner is dead. POSIX has
# one pid namespace, but records its derivation too so newly written markers
# stay self-describing without changing legacy-reader behavior.
oms_process_native_pid_source() {
  case "$(uname -s 2>/dev/null || true)" in
    MINGW*|MSYS*|CYGWIN*) printf 'msys-proc-v1\n' ;;
    *) printf 'logical-pid-v1\n' ;;
  esac
}

# A PID alone is not an identity: after a crash the OS may reuse it while the
# mkdir lock remains. Record a process-start token where the host exposes one,
# and compare it before treating a live PID as the original holder. Linux's
# /proc token is numeric and boot-relative; the portable fallback is the
# locale-stable ps start time used by macOS/BSD and Git Bash when available.
oms_file_lock_process_start_token() {
  local pid="$1"
  local line=""
  local rest=""
  local start_value=""

  case "$pid" in
    *[!0-9]*|"") return 1 ;;
  esac

  if [ -r "/proc/$pid/stat" ]; then
    line="$(sed -n '1p' "/proc/$pid/stat" 2>/dev/null || true)"
    # comm is parenthesized and may contain spaces or ')' characters. Remove
    # through the final ') '; the twentieth remaining field is starttime (22).
    case "$line" in
      *') '*)
        rest="${line##*) }"
        # shellcheck disable=SC2086 # /proc fields are deliberately split.
        set -- $rest
        if [ "$#" -ge 20 ]; then
          shift 19
          case "$1" in
            *[!0-9]*|"") ;;
            *) printf 'proc:%s\n' "$1"; return 0 ;;
          esac
        fi
        ;;
    esac
  fi

  start_value="$(LC_ALL=C ps -p "$pid" -o lstart= 2>/dev/null |
    tr -d '\r' | awk '{$1=$1; print; exit}' || true)"
  [ -n "$start_value" ] || return 1
  printf 'ps:%s\n' "$start_value"
}

OMS_FILE_LOCK_CURRENT_PID=""
OMS_FILE_LOCK_CURRENT_START=""

oms_file_lock_set_current_identity() {
  local pid=""

  # BASHPID identifies the current subshell on modern Bash. Stock Bash 3.2 has
  # no BASHPID and $$ remains the parent's value inside (...), so ask a
  # short-lived child for its PPID. The child must BECOME the substitution
  # fork (exec) so its PPID is this shell: without exec, real Bash 3.2 hands
  # sh an ephemeral intermediate fork and every call returns a different,
  # already-dead pid — stale detection then displaces live locks (the
  # 2026-08-10 macOS lifecycle-lock leak was this pattern one file over).
  if [ "${BASH_VERSINFO[0]:-0}" -ge 4 ] 2>/dev/null &&
      [ -n "${BASHPID:-}" ]; then
    pid="$BASHPID"
  else
    pid="$(exec sh -c 'printf "%s\n" "$PPID"' 2>/dev/null)" || pid=""
    pid="${pid//$'\r'/}"
    [ -n "$pid" ] || return 75
  fi
  case "$pid" in
    *[!0-9]*|"") return 75 ;;
  esac
  OMS_FILE_LOCK_CURRENT_PID="$pid"
  OMS_FILE_LOCK_CURRENT_START="$(
    oms_file_lock_process_start_token "$OMS_FILE_LOCK_CURRENT_PID" 2>/dev/null || true
  )"
  OMS_FILE_LOCK_CURRENT_START="${OMS_FILE_LOCK_CURRENT_START//$'\r'/}"
}

oms_file_lock_mkdir_write_identity() {
  local lock_dir="$1"
  local owner_id="$2"

  oms_file_lock_set_current_identity
  # owner is the immutable generation discriminator used by stale reclaimers.
  # Write it first so a crash during metadata initialization cannot make two
  # complete generations look alike.
  {
    printf '%s\n' "$owner_id" > "$lock_dir/owner"
    printf '%s\n' "$OMS_FILE_LOCK_CURRENT_PID" > "$lock_dir/pid"
    if [ -n "$OMS_FILE_LOCK_CURRENT_START" ]; then
      printf '%s\n' "$OMS_FILE_LOCK_CURRENT_START" > "$lock_dir/process-start"
    fi
    printf '%s\n' "$(date +%s)" > "$lock_dir/started"
  } || {
    rm -rf "$lock_dir"
    return 75
  }
}

oms_file_lock_mkdir_stale() {
  local lock_dir="$1"
  local timeout="$2"
  local now="$3"
  local pid=""
  local saved_process_start=""
  local current_process_start=""
  local owner=""
  local started=""

  [ -d "$lock_dir" ] || return 1
  [ -f "$lock_dir/owner" ] && owner="$(sed -n '1p' "$lock_dir/owner" 2>/dev/null || true)"
  [ -f "$lock_dir/pid" ] && pid="$(sed -n '1p' "$lock_dir/pid" 2>/dev/null || true)"
  case "$pid" in
    *[!0-9]*|"") ;;
    *)
      if ! oms_file_lock_pid_alive "$pid"; then
        return 0
      fi
      [ -f "$lock_dir/process-start" ] &&
        saved_process_start="$(sed -n '1p' "$lock_dir/process-start" 2>/dev/null || true)"
      if [ -n "$saved_process_start" ]; then
        current_process_start="$(
          oms_file_lock_process_start_token "$pid" 2>/dev/null || true
        )"
        current_process_start="${current_process_start//$'\r'/}"
        if [ -n "$current_process_start" ] &&
            [ "$current_process_start" != "$saved_process_start" ]; then
          return 0
        fi
      fi
      # A live matching holder is authoritative regardless of lock age. The
      # timeout bounds waiting; it is never a lease expiry.
      return 1
      ;;
  esac

  [ -f "$lock_dir/started" ] && started="$(sed -n '1p' "$lock_dir/started" 2>/dev/null || true)"
  case "$started" in
    *[!0-9]*|"") return 1 ;;
  esac
  # Missing/partial legacy metadata is reclaimable by age only when it has an
  # owner generation that contenders can compare before rename.
  [ -n "$owner" ] || return 1
  [ $((now - started)) -ge "$timeout" ]
}

oms_file_lock_mkdir_generation() {
  local lock_dir="$1"
  local owner=""
  local pid=""
  local process_start=""
  local started=""

  [ -d "$lock_dir" ] || return 1
  [ -f "$lock_dir/owner" ] && owner="$(sed -n '1p' "$lock_dir/owner" 2>/dev/null || true)"
  if [ -n "$owner" ]; then
    printf 'owner:%s\n' "$owner"
    return 0
  fi

  [ -f "$lock_dir/pid" ] && pid="$(sed -n '1p' "$lock_dir/pid" 2>/dev/null || true)"
  [ -f "$lock_dir/process-start" ] &&
    process_start="$(sed -n '1p' "$lock_dir/process-start" 2>/dev/null || true)"
  [ -f "$lock_dir/started" ] && started="$(sed -n '1p' "$lock_dir/started" 2>/dev/null || true)"
  [ -n "$pid$process_start$started" ] || return 1
  printf 'legacy:%s:%s:%s\n' "$pid" "$process_start" "$started"
}

# A stale-recovery contender is a unique bakery claim. A PID/start-token in the
# claim name makes even a crash before metadata initialization observable. Dead
# claims are ignored, so killing an elected reclaimer cannot wedge a generation.
oms_file_lock_reclaim_claim_live() {
  local claim_dir="$1"
  local claim_name=""
  local pid=""
  local saved_start_sum=""
  local current_start=""
  local current_start_sum=""
  local remainder=""

  [ -d "$claim_dir" ] && [ ! -L "$claim_dir" ] || return 1
  claim_name="$(basename "$claim_dir")"
  pid="${claim_name%%.*}"
  remainder="${claim_name#*.}"
  [ "$remainder" != "$claim_name" ] || return 1
  saved_start_sum="${remainder%%.*}"
  case "$pid" in
    *[!0-9]*|"") return 1 ;;
  esac
  oms_file_lock_pid_alive "$pid" || return 1

  if [ "$saved_start_sum" != none ]; then
    current_start="$(oms_file_lock_process_start_token "$pid" 2>/dev/null || true)"
    current_start="${current_start//$'\r'/}"
    if [ -n "$current_start" ]; then
      current_start_sum="$(printf '%s' "$current_start" | cksum |
        awk '{print $1 "-" $2}')"
      [ "$current_start_sum" = "$saved_start_sum" ] || return 1
    fi
  fi
  return 0
}

# Reclaim one observed stale generation. Lamport bakery claims elect one live
# observer without a crash-prone canonical guard. A loser must re-read the
# canonical path, so it can never rename the winner's newly-created live lock.
oms_file_lock_mkdir_reclaim() {
  local lock_dir="$1"
  local timeout="$2"
  local now="$3"
  local generation=""
  local current_generation=""
  local reclaim_queue=""
  local claim_name=""
  local claim_dir=""
  local start_sum="none"
  local ticket=1
  local max_ticket=0
  local other=""
  local other_name=""
  local other_pid=""
  local other_ticket=""
  local choosing=""
  local blocked=0
  local stale_dir=""
  local reclaimed=1

  oms_file_lock_mkdir_stale "$lock_dir" "$timeout" "$now" || return 1
  generation="$(oms_file_lock_mkdir_generation "$lock_dir" 2>/dev/null || true)"
  generation="${generation//$'\r'/}"
  [ -n "$generation" ] || return 1

  oms_file_lock_set_current_identity || return 1
  if [ -n "$OMS_FILE_LOCK_CURRENT_START" ]; then
    start_sum="$(printf '%s' "$OMS_FILE_LOCK_CURRENT_START" | cksum |
      awk '{print $1 "-" $2}')"
  fi
  reclaim_queue="$lock_dir.reclaim-queue"
  mkdir -p "$reclaim_queue" 2>/dev/null || return 1
  [ -d "$reclaim_queue" ] && [ ! -L "$reclaim_queue" ] || return 1
  claim_name="$OMS_FILE_LOCK_CURRENT_PID.$start_sum.$(date +%s).${RANDOM:-0}"
  claim_dir="$reclaim_queue/$claim_name"
  mkdir "$claim_dir" 2>/dev/null || return 1
  if ! printf '1\n' > "$claim_dir/choosing" ||
     ! printf '%s\n' "$generation" > "$claim_dir/generation"; then
    rm -rf "$claim_dir"
    return 1
  fi

  # Choose one greater than the currently published ticket. Simultaneous
  # choosers may tie; their distinct numeric PIDs provide the total order.
  for other in "$reclaim_queue"/*; do
    [ "$other" != "$claim_dir" ] || continue
    oms_file_lock_reclaim_claim_live "$other" || continue
    other_ticket="$(sed -n '1p' "$other/ticket" 2>/dev/null || true)"
    case "$other_ticket" in
      *[!0-9]*|"") continue ;;
    esac
    [ "$other_ticket" -le "$max_ticket" ] || max_ticket="$other_ticket"
  done
  ticket=$((max_ticket + 1))
  if ! printf '%s\n' "$ticket" > "$claim_dir/ticket" ||
     ! printf '0\n' > "$claim_dir/choosing"; then
    rm -rf "$claim_dir"
    return 1
  fi

  for other in "$reclaim_queue"/*; do
    [ "$other" != "$claim_dir" ] || continue
    oms_file_lock_reclaim_claim_live "$other" || continue
    choosing="$(sed -n '1p' "$other/choosing" 2>/dev/null || true)"
    other_ticket="$(sed -n '1p' "$other/ticket" 2>/dev/null || true)"
    case "$other_ticket" in
      *[!0-9]*|"") blocked=1; break ;;
    esac
    [ "$choosing" = 0 ] || { blocked=1; break; }
    other_name="$(basename "$other")"
    other_pid="${other_name%%.*}"
    if [ "$other_ticket" -lt "$ticket" ] ||
       { [ "$other_ticket" -eq "$ticket" ] &&
         [ "$other_pid" -lt "$OMS_FILE_LOCK_CURRENT_PID" ]; }; then
      blocked=1
      break
    fi
  done
  if [ "$blocked" = 1 ]; then
    rm -rf "$claim_dir"
    return 1
  fi

  current_generation="$(oms_file_lock_mkdir_generation "$lock_dir" 2>/dev/null || true)"
  current_generation="${current_generation//$'\r'/}"
  if [ "$current_generation" = "$generation" ] &&
      oms_file_lock_mkdir_stale "$lock_dir" "$timeout" "$now"; then
    stale_dir="$lock_dir.stale.$OMS_FILE_LOCK_CURRENT_PID.$(date +%s).${RANDOM:-0}"
    if [ ! -e "$stale_dir" ] && [ ! -L "$stale_dir" ] &&
        mv "$lock_dir" "$stale_dir" 2>/dev/null; then
      rm -rf "$stale_dir"
      reclaimed=0
    fi
  fi
  rm -rf "$claim_dir"
  return "$reclaimed"
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
  local elapsed
  local remaining

  start="$(date +%s)"
  while :; do
    if mkdir "$lock_dir" 2>/dev/null; then
      oms_file_lock_mkdir_write_identity "$lock_dir" "$owner_id" || return $?
      return 0
    fi

    now="$(date +%s)"
    elapsed=$((now - start))
    if oms_file_lock_mkdir_reclaim "$lock_dir" "$timeout" "$now"; then
      continue
    fi

    if [ "$elapsed" -ge "$timeout" ]; then
      echo "error: could not acquire lock for $state_file after ${timeout}s: $lock_dir" >&2
      return 75
    fi
    remaining=$((timeout - elapsed))
    oms_poll_sleep_labeled file-lock-mkdir "$elapsed" "$remaining"
  done
}

oms_with_file_lock_mkdir() {
  local state_file="$1"
  local lock_dir="$2"
  local timeout="$3"
  local owner_id
  shift 3

  (
    oms_file_lock_set_current_identity
    owner_id="$OMS_FILE_LOCK_CURRENT_PID.$(date +%s).${RANDOM:-0}"
    oms_file_lock_mkdir_acquire "$state_file" "$lock_dir" "$timeout" "$owner_id" || exit $?
    # An errexit unwind runs EXIT after this function's local scope is gone.
    # Freeze cleanup identity in this lock subshell; a nested lock gets a child
    # copy and therefore cannot overwrite its parent's release target.
    OMS_FILE_LOCK_CLEANUP_DIR="$lock_dir"
    OMS_FILE_LOCK_CLEANUP_OWNER="$owner_id"
    lock_cleanup_done=0
    oms_file_lock_cleanup() {
      [ "$lock_cleanup_done" = 0 ] || return 0
      lock_cleanup_done=1
      oms_file_lock_mkdir_release \
        "$OMS_FILE_LOCK_CLEANUP_DIR" "$OMS_FILE_LOCK_CLEANUP_OWNER"
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

  while :; do
    if mkdir "$lock_dir" 2>/dev/null; then
      oms_file_lock_mkdir_write_identity "$lock_dir" "$owner_id" || return $?
      return 0
    fi

    now="$(date +%s)"
    if oms_file_lock_mkdir_reclaim "$lock_dir" "$timeout" "$now"; then
      continue
    fi

    return 75
  done
}

oms_try_file_lock_mkdir() {
  local lock_dir="$2"
  local timeout="$3"
  local owner_id
  shift 3

  (
    oms_file_lock_set_current_identity
    owner_id="$OMS_FILE_LOCK_CURRENT_PID.$(date +%s).${RANDOM:-0}"
    oms_try_file_lock_mkdir_acquire "$lock_dir" "$timeout" "$owner_id" || exit $?
    OMS_FILE_LOCK_CLEANUP_DIR="$lock_dir"
    OMS_FILE_LOCK_CLEANUP_OWNER="$owner_id"
    lock_cleanup_done=0
    oms_file_lock_cleanup() {
      [ "$lock_cleanup_done" = 0 ] || return 0
      lock_cleanup_done=1
      oms_file_lock_mkdir_release \
        "$OMS_FILE_LOCK_CLEANUP_DIR" "$OMS_FILE_LOCK_CLEANUP_OWNER"
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

# Hold a lock for the rest of the caller's process instead of around one
# command. oms_with_file_lock runs its command in a subshell, which is right for
# a read-modify-write on one file and wrong for a multi-step operation whose
# later steps need variables the earlier steps set — landing a patch runs a
# whole admission gate between its clean-tree check and its apply.
# Non-blocking on purpose: a second lander cannot usefully queue, because the
# tree it was admitted against is the tree the first lander is changing.
# Returns 75 when someone else holds it. Release with
# oms_release_held_file_lock (a flock fd is released by process exit anyway).
OMS_HELD_LOCK_KIND=""
OMS_HELD_LOCK_DIR=""
OMS_HELD_LOCK_OWNER=""

oms_hold_file_lock() {
  local state_file="$1"
  local fd="${2:-7}"
  local lock_path
  local owner_id

  [ -n "$state_file" ] || {
    echo "error: lock target is required" >&2
    return 2
  }
  lock_path="$(oms_file_lock_path_for_file "$state_file")"
  mkdir -p "$(dirname "$lock_path")"

  if command -v flock >/dev/null 2>&1 && [ "${OMS_LOCK_FORCE_MKDIR:-0}" != "1" ]; then
    eval "exec $fd>\"\$lock_path\"" || return 75
    flock -n "$fd" || return 75
    OMS_HELD_LOCK_KIND="flock"
    return 0
  fi
  oms_file_lock_set_current_identity
  owner_id="$OMS_FILE_LOCK_CURRENT_PID.$(date +%s).${RANDOM:-0}"
  oms_try_file_lock_mkdir_acquire "$lock_path" "$(oms_file_lock_timeout)" "$owner_id" ||
    return 75
  OMS_HELD_LOCK_KIND="mkdir"
  OMS_HELD_LOCK_DIR="$lock_path"
  OMS_HELD_LOCK_OWNER="$owner_id"
}

oms_release_held_file_lock() {
  [ "$OMS_HELD_LOCK_KIND" = "mkdir" ] || return 0
  oms_file_lock_mkdir_release "$OMS_HELD_LOCK_DIR" "$OMS_HELD_LOCK_OWNER"
  OMS_HELD_LOCK_KIND=""
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
  local elapsed
  local remaining
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
        elapsed=$((now - start))
        if [ "$elapsed" -ge "$timeout" ]; then
          echo "error: could not acquire lock for $state_file after ${timeout}s: $lock_path" >&2
          exit 75
        fi
        remaining=$((timeout - elapsed))
        oms_poll_sleep_labeled file-lock "$elapsed" "$remaining"
      done
      "$@"
    )
  else
    oms_with_file_lock_mkdir "$state_file" "$lock_path" "$timeout" "$@"
  fi
}
