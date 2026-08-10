#!/usr/bin/env bash
# shellcheck shell=bash

# One user-wide mutation lock for the canonical install lifecycle. It is a
# mkdir lock even where flock exists: install.sh must carry ownership across an
# exec into a freshly cloned checkout, and a file descriptor is not a portable
# cross-exec contract on Bash 3.2 and Windows Git Bash.

OMS_INSTALL_LIFECYCLE_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=scripts/lib/file-lock.sh
. "$OMS_INSTALL_LIFECYCLE_LIB_DIR/file-lock.sh"

OMS_INSTALL_LIFECYCLE_LOCK_LOCAL=0
OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH=""
OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_OWNER=""
OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID=""
OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID_START=""
OMS_INSTALL_LIFECYCLE_RECOVERY_CLAIM_LOCAL_PATH=""
OMS_INSTALL_LIFECYCLE_RECOVERY_CLAIM_LOCAL_OWNER=""

oms_install_lifecycle_recovery_claim_release() {
  local recorded=""
  local claim="$OMS_INSTALL_LIFECYCLE_RECOVERY_CLAIM_LOCAL_PATH"
  [ -n "$claim" ] || return 0
  if [ -d "$claim" ] && [ ! -L "$claim" ]; then
    recorded="$(oms_install_lifecycle_lock_read "$claim/owner")"
    if [ "$recorded" = "$OMS_INSTALL_LIFECYCLE_RECOVERY_CLAIM_LOCAL_OWNER" ]; then
      rm -f "$claim/pid" "$claim/pid-start" "$claim/started" "$claim/owner"
      rmdir "$claim" 2>/dev/null || return 75
      rmdir "$(dirname "$claim")" 2>/dev/null || true
    fi
  fi
  OMS_INSTALL_LIFECYCLE_RECOVERY_CLAIM_LOCAL_PATH=""
  OMS_INSTALL_LIFECYCLE_RECOVERY_CLAIM_LOCAL_OWNER=""
}

# $$ is deliberately stable across a Bash (...) subshell, so it is not a
# process identity for lock ownership. BASHPID is exact where Bash provides it;
# Bash 3.2 falls back to the PPID observed by a short-lived child process.
oms_install_lifecycle_lock_current_identity() {
  local pid=""
  local process_start=""

  # BASHPID became a shell-owned identity after the stock Bash 3.2 baseline.
  # Ignore an environment variable with that name on older Bash releases.
  if [ "${BASH_VERSINFO[0]:-0}" -ge 4 ] 2>/dev/null &&
     [ -n "${BASHPID:-}" ]; then
    pid="$BASHPID"
  else
    # The probe child must BECOME the substitution fork (exec), so its PPID
    # is this shell. Without exec, stock Bash 3.2 gives sh a fresh
    # intermediate fork as parent: every call then returns a different,
    # already-dead pid, adopt and release never match their own lock, and
    # the installer leaks it after stale-taking-over itself (the 2026-08-10
    # macOS e2e leak). Modern Bash happens to exec substitutions anyway,
    # which is why only real 3.2 hosts saw it.
    pid="$(exec sh -c 'printf "%s\n" "$PPID"' 2>/dev/null)" || pid=""
    pid="${pid//$'\r'/}"
    [ -n "$pid" ] || return 75
  fi
  case "$pid" in
    *[!0-9]*|"") return 75 ;;
  esac
  process_start="$(oms_install_lifecycle_lock_process_start "$pid")"
  process_start="${process_start//$'\r'/}"
  OMS_INSTALL_LIFECYCLE_CURRENT_PID="$pid"
  OMS_INSTALL_LIFECYCLE_CURRENT_PID_START="$process_start"
}

# A live PID can be reused after a crash. Record a start token when the host
# exposes one; otherwise the random owner token and live-PID rule remain the
# portable fallback. /proc covers Linux and ps covers BSD/macOS. Git Bash
# emulates exec across Windows processes, so PID alone is its stable contract.
oms_install_lifecycle_lock_process_start() {
  local pid="$1"
  local line=""
  local rest=""
  local value=""

  case "$(uname -s 2>/dev/null || true)" in
    MINGW*|MSYS*|CYGWIN*) return 0 ;;
  esac

  if [ -r "/proc/$pid/stat" ]; then
    IFS= read -r line < "/proc/$pid/stat" || line=""
    rest="${line##*) }"
    if [ "$rest" != "$line" ]; then
      # After the closing command-name parenthesis, word 20 is proc field 22.
      value="$(printf '%s\n' "$rest" | awk 'NF >= 20 { print $20; exit }')"
      case "$value" in
        *[!0-9]*|"") ;;
        *) printf 'proc:%s\n' "$value"; return 0 ;;
      esac
    fi
  fi
  value="$(LC_ALL=C ps -p "$pid" -o lstart= 2>/dev/null || true)"
  value="$(printf '%s\n' "$value" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e 's/[[:space:]][[:space:]]*/ /g')"
  [ -n "$value" ] && printf 'ps:%s\n' "$value"
}

oms_install_lifecycle_lock_path() {
  local raw="${OMS_INSTALL_LIFECYCLE_LOCK:-}"
  local parent
  local name

  if [ -z "$raw" ]; then
    raw="$(oms_file_lock_dir)/install-lifecycle.lock.d"
  fi
  case "$raw" in
    /*) ;;
    *) raw="$PWD/$raw" ;;
  esac
  parent="$(dirname "$raw")"
  name="$(basename "$raw")"
  if [ "$name" != "install-lifecycle.lock.d" ]; then
    echo "error: invalid install lifecycle lock path: $raw" >&2
    return 75
  fi
  mkdir -p "$parent" || return
  parent="$(cd "$parent" && pwd -P)" || return
  printf '%s/%s\n' "$parent" "$name"
}

oms_install_lifecycle_lock_timeout() {
  local timeout="${OMS_INSTALL_LIFECYCLE_LOCK_TIMEOUT:-${OMS_LOCK_TIMEOUT:-300}}"

  case "$timeout" in
    *[!0-9]*|"") timeout=300 ;;
  esac
  [ "$timeout" -gt 0 ] || timeout=300
  printf '%s\n' "$timeout"
}

oms_install_lifecycle_lock_read() {
  local path="$1"
  local value=""

  [ -f "$path" ] && value="$(sed -n '1p' "$path" 2>/dev/null || true)"
  printf '%s\n' "${value//$'\r'/}"
}

oms_install_lifecycle_recovery_claim_stale() {  # CLAIM TIMEOUT NOW
  local claim="$1" timeout="$2" now="$3"
  local pid pid_start actual_start started
  pid="$(oms_install_lifecycle_lock_read "$claim/pid")"
  pid_start="$(oms_install_lifecycle_lock_read "$claim/pid-start")"
  started="$(oms_install_lifecycle_lock_read "$claim/started")"
  case "$pid" in
    *[!0-9]*|"")
      case "$started" in
        *[!0-9]*|"")
          started="$(basename "$claim")"
          started="${started%%.*}"
          case "$started" in *[!0-9]*|"") return 1 ;; esac
          [ $((now - started)) -ge "$timeout" ]
          ;;
        *) [ $((now - started)) -ge "$timeout" ] ;;
      esac
      ;;
    *)
      if ! kill -0 "$pid" 2>/dev/null; then
        return 0
      fi
      if [ -n "$pid_start" ]; then
        actual_start="$(oms_install_lifecycle_lock_process_start "$pid")"
        actual_start="${actual_start//$'\r'/}"
        [ -n "$actual_start" ] && [ "$actual_start" != "$pid_start" ]
        return $?
      fi
      return 1
      ;;
  esac
}

oms_install_lifecycle_recovery_claim_acquire() {  # LOCK_PATH TIMEOUT OWNER START
  local path="$1" timeout="$2" owner="$3" start="$4"
  local root="$1.recovery-claims.d" name claim candidate candidate_name
  local now elapsed remaining first tick_owner
  local -a names=()

  if [ -L "$root" ]; then return 75; fi
  mkdir -p "$root" || return 75
  [ -d "$root" ] && [ ! -L "$root" ] || return 75
  name="$(date +%s).$owner"
  claim="$root/$name"
  mkdir "$claim" 2>/dev/null || return 75
  if ! printf '%s\n' "$OMS_INSTALL_LIFECYCLE_CURRENT_PID" > "$claim/pid" ||
     ! printf '%s\n' "$OMS_INSTALL_LIFECYCLE_CURRENT_PID_START" > "$claim/pid-start" ||
     ! printf '%s\n' "$(date +%s)" > "$claim/started" ||
     ! printf '%s\n' "$owner" > "$claim/owner"; then
    rm -f "$claim/pid" "$claim/pid-start" "$claim/started" "$claim/owner"
    rmdir "$claim" 2>/dev/null || true
    return 75
  fi
  OMS_INSTALL_LIFECYCLE_RECOVERY_CLAIM_LOCAL_PATH="$claim"
  OMS_INSTALL_LIFECYCLE_RECOVERY_CLAIM_LOCAL_OWNER="$owner"

  while :; do
    now="$(date +%s)"
    elapsed=$((now - start))
    names=()
    for candidate in "$root"/*; do
      [ -e "$candidate" ] || continue
      [ -d "$candidate" ] && [ ! -L "$candidate" ] || {
        oms_install_lifecycle_recovery_claim_release || true
        return 75
      }
      if [ "$candidate" != "$claim" ] &&
         oms_install_lifecycle_recovery_claim_stale "$candidate" "$timeout" "$now"; then
        tick_owner="$(oms_install_lifecycle_lock_read "$candidate/owner")"
        rm -f "$candidate/pid" "$candidate/pid-start" \
          "$candidate/started" "$candidate/owner"
        rmdir "$candidate" 2>/dev/null || true
        [ ! -e "$candidate" ] || [ "$(oms_install_lifecycle_lock_read \
          "$candidate/owner")" != "$tick_owner" ] || return 75
      fi
      [ -d "$candidate" ] || continue
      candidate_name="$(basename "$candidate")"
      names+=("$candidate_name")
    done
    first="$(printf '%s\n' "${names[@]}" | LC_ALL=C sort | sed -n '1p')"
    if [ "$first" = "$name" ] && [ -d "$claim" ] &&
       [ "$(oms_install_lifecycle_lock_read "$claim/owner")" = "$owner" ]; then
      return 0
    fi
    [ "$elapsed" -lt "$timeout" ] || {
      oms_install_lifecycle_recovery_claim_release || true
      return 75
    }
    remaining=$((timeout - elapsed))
    oms_poll_sleep_labeled install-lifecycle-recovery-claim "$elapsed" "$remaining"
  done
}

oms_install_lifecycle_lock_adopt() {
  local path="$1"
  local recorded_owner
  local recorded_pid
  local recorded_pid_start

  [ "${OMS_INSTALL_LIFECYCLE_LOCK_HELD:-0}" = 1 ] || return 1
  [ "${OMS_INSTALL_LIFECYCLE_LOCK_PATH:-}" = "$path" ] || return 1
  [ -d "$path" ] && [ ! -L "$path" ] || return 1
  recorded_owner="$(oms_install_lifecycle_lock_read "$path/owner")"
  recorded_pid="$(oms_install_lifecycle_lock_read "$path/pid")"
  recorded_pid_start="$(oms_install_lifecycle_lock_read "$path/pid-start")"
  [ -n "${OMS_INSTALL_LIFECYCLE_LOCK_OWNER:-}" ] || return 1
  [ "$recorded_owner" = "$OMS_INSTALL_LIFECYCLE_LOCK_OWNER" ] || return 1
  oms_install_lifecycle_lock_current_identity || return 1
  # exec preserves the actual PID and start token; a child merely inheriting
  # the environment does not. This keeps the marker non-transferable.
  [ "$recorded_pid" = "$OMS_INSTALL_LIFECYCLE_CURRENT_PID" ] || return 1
  if [ -n "$recorded_pid_start" ]; then
    [ -n "$OMS_INSTALL_LIFECYCLE_CURRENT_PID_START" ] || return 1
    [ "$recorded_pid_start" = "$OMS_INSTALL_LIFECYCLE_CURRENT_PID_START" ] || return 1
  fi

  OMS_INSTALL_LIFECYCLE_LOCK_LOCAL=1
  OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH="$path"
  OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_OWNER="$recorded_owner"
  OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID="$recorded_pid"
  OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID_START="$recorded_pid_start"
}

oms_install_lifecycle_lock_mkdir_acquire() {
  local path="$1"
  local timeout="$2"
  local owner="$3"
  local start
  local now
  local elapsed
  local remaining
  local recorded_pid
  local recorded_pid_start
  local actual_pid_start
  local recorded_started
  local stale=0
  local stale_path
  local recorded_owner
  local snapshot
  local current_snapshot

  oms_install_lifecycle_lock_initialize() {
    if ! printf '%s\n' "$OMS_INSTALL_LIFECYCLE_CURRENT_PID" > "$path/pid" ||
      ! printf '%s\n' "$OMS_INSTALL_LIFECYCLE_CURRENT_PID_START" > "$path/pid-start" ||
      ! printf '%s\n' "$(date +%s)" > "$path/started" ||
      ! printf '%s\n' "$owner" > "$path/owner"; then
      rm -rf "$path"
      return 75
    fi
  }

  start="$(date +%s)"
  oms_install_lifecycle_recovery_claim_acquire "$path" "$timeout" "$owner" "$start" ||
    return 75
  while :; do
    now="$(date +%s)"
    elapsed=$((now - start))
    if mkdir "$path" 2>/dev/null; then
      if oms_install_lifecycle_lock_initialize; then
        oms_install_lifecycle_recovery_claim_release || return 75
        return 0
      fi
      oms_install_lifecycle_recovery_claim_release || true
      return 75
    fi

    # An active lifecycle can legitimately take longer than the wait timeout.
    # A live PID is therefore never stale; only a dead owner (or incomplete
    # metadata left behind past the grace period) may be recovered.
    if [ -L "$path" ] || [ ! -d "$path" ]; then
      oms_install_lifecycle_recovery_claim_release || true
      return 75
    fi
    recorded_pid="$(oms_install_lifecycle_lock_read "$path/pid")"
    recorded_owner="$(oms_install_lifecycle_lock_read "$path/owner")"
    recorded_pid_start="$(oms_install_lifecycle_lock_read "$path/pid-start")"
    recorded_started="$(oms_install_lifecycle_lock_read "$path/started")"
    snapshot="$recorded_owner|$recorded_pid|$recorded_pid_start|$recorded_started"
    stale=0
    case "$recorded_pid" in
      *[!0-9]*|"")
        case "$recorded_started" in
          *[!0-9]*|"")
            [ "$elapsed" -ge "$timeout" ] && stale=1
            ;;
          *)
            if [ "$elapsed" -ge "$timeout" ] ||
               [ $((now - recorded_started)) -ge "$timeout" ]; then
              stale=1
            fi
            ;;
        esac
        ;;
      *)
        if ! kill -0 "$recorded_pid" 2>/dev/null; then
          stale=1
        else
          if [ -n "$recorded_pid_start" ]; then
            actual_pid_start="$(oms_install_lifecycle_lock_process_start "$recorded_pid")"
            actual_pid_start="${actual_pid_start//$'\r'/}"
            if [ -n "$actual_pid_start" ] &&
               [ "$recorded_pid_start" != "$actual_pid_start" ]; then
              stale=1
            fi
          fi
        fi
        ;;
    esac

    if [ "$stale" = 1 ]; then
      if [ -n "${OMS_TEST_INSTALL_LIFECYCLE_STALE_BARRIER:-}" ]; then
        printf '%s\n' "$OMS_INSTALL_LIFECYCLE_CURRENT_PID" > \
          "$OMS_TEST_INSTALL_LIFECYCLE_STALE_BARRIER.ready"
        while [ ! -e "$OMS_TEST_INSTALL_LIFECYCLE_STALE_BARRIER.release" ]; do
          sleep 1
        done
      fi
      current_snapshot="$(
        printf '%s|%s|%s|%s\n' \
          "$(oms_install_lifecycle_lock_read "$path/owner")" \
          "$(oms_install_lifecycle_lock_read "$path/pid")" \
          "$(oms_install_lifecycle_lock_read "$path/pid-start")" \
          "$(oms_install_lifecycle_lock_read "$path/started")"
      )"
      stale_path="$path.stale.$OMS_INSTALL_LIFECYCLE_CURRENT_PID.$now.${RANDOM:-0}"
      if [ "$current_snapshot" = "$snapshot" ] &&
         mv "$path" "$stale_path" 2>/dev/null; then
        rm -rf "$stale_path"
        if mkdir "$path" 2>/dev/null && oms_install_lifecycle_lock_initialize; then
          oms_install_lifecycle_recovery_claim_release || return 75
          return 0
        fi
      fi
    fi

    [ "$elapsed" -lt "$timeout" ] || {
      oms_install_lifecycle_recovery_claim_release || true
      return 75
    }
    remaining=$((timeout - elapsed))
    oms_poll_sleep_labeled install-lifecycle-lock "$elapsed" "$remaining"
  done
}

oms_install_lifecycle_lock_acquire() {
  local action="${1:-mutate the install}"
  local path
  local owner
  local status=0

  path="$(oms_install_lifecycle_lock_path)" || return 75
  if oms_install_lifecycle_lock_adopt "$path"; then
    return 0
  fi
  if [ -L "$path" ]; then
    echo "error: install lifecycle lock must not be a symbolic link: $path" >&2
    return 75
  fi

  oms_install_lifecycle_lock_current_identity || {
    echo "error: could not determine install lifecycle process identity" >&2
    return 75
  }
  owner="$OMS_INSTALL_LIFECYCLE_CURRENT_PID.$(date +%s).${RANDOM:-0}"
  oms_install_lifecycle_lock_mkdir_acquire "$path" \
    "$(oms_install_lifecycle_lock_timeout)" "$owner" || status=$?
  if [ "$status" -ne 0 ]; then
    echo "error: install lifecycle lock is busy while trying to $action: $path" >&2
    return "$status"
  fi

  OMS_INSTALL_LIFECYCLE_LOCK_LOCAL=1
  OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH="$path"
  OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_OWNER="$owner"
  OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID="$OMS_INSTALL_LIFECYCLE_CURRENT_PID"
  OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID_START="$OMS_INSTALL_LIFECYCLE_CURRENT_PID_START"
  OMS_INSTALL_LIFECYCLE_LOCK_HELD=1
  OMS_INSTALL_LIFECYCLE_LOCK_PATH="$path"
  OMS_INSTALL_LIFECYCLE_LOCK_OWNER="$owner"
  export OMS_INSTALL_LIFECYCLE_LOCK_HELD OMS_INSTALL_LIFECYCLE_LOCK_PATH \
    OMS_INSTALL_LIFECYCLE_LOCK_OWNER
}

oms_install_lifecycle_lock_release() {
  local recorded_owner
  local recorded_pid
  local recorded_pid_start

  oms_install_lifecycle_recovery_claim_release || true
  [ "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL" = 1 ] || return 0
  oms_install_lifecycle_lock_current_identity || return 0
  [ "$OMS_INSTALL_LIFECYCLE_CURRENT_PID" = "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID" ] ||
    return 0
  if [ -n "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID_START" ]; then
    [ "$OMS_INSTALL_LIFECYCLE_CURRENT_PID_START" = \
      "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID_START" ] || return 0
  fi
  if [ -d "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH" ] &&
     [ ! -L "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH" ]; then
    recorded_owner="$(oms_install_lifecycle_lock_read \
      "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH/owner")"
    recorded_pid="$(oms_install_lifecycle_lock_read \
      "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH/pid")"
    recorded_pid_start="$(oms_install_lifecycle_lock_read \
      "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH/pid-start")"
    if [ "$recorded_owner" = "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_OWNER" ] &&
       [ "$recorded_pid" = "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID" ] &&
       [ "$recorded_pid_start" = "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID_START" ]; then
      rm -f "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH/pid" \
        "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH/pid-start" \
        "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH/started" \
        "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH/owner"
      rmdir "$OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH" 2>/dev/null ||
        echo "warning: install lifecycle lock directory was not empty: $OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH" >&2
    fi
  fi
  OMS_INSTALL_LIFECYCLE_LOCK_LOCAL=0
  OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PATH=""
  OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_OWNER=""
  OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID=""
  OMS_INSTALL_LIFECYCLE_LOCK_LOCAL_PID_START=""
  unset OMS_INSTALL_LIFECYCLE_LOCK_HELD OMS_INSTALL_LIFECYCLE_LOCK_PATH \
    OMS_INSTALL_LIFECYCLE_LOCK_OWNER
}
