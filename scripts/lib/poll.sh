# shellcheck shell=bash
# Adaptive polling helpers. Sourced, not executed.

oms_poll_int_or_default() {
  local value="$1"
  local default="$2"

  case "$value" in
    *[!0-9]*|"") printf '%s\n' "$default" ;;
    *) printf '%s\n' "$value" ;;
  esac
}

oms_poll_interval_seconds() {
  local elapsed
  local remaining
  local max_sleep
  local interval

  elapsed="$(oms_poll_int_or_default "${1:-0}" 0)"
  remaining="${2:-}"
  max_sleep="$(oms_poll_int_or_default "${OMS_POLL_MAX_SECONDS:-10}" 10)"
  [ "$max_sleep" -gt 0 ] || max_sleep=10

  if [ "$elapsed" -lt 5 ]; then
    interval=1
  elif [ "$elapsed" -lt 30 ]; then
    interval=2
  elif [ "$elapsed" -lt 120 ]; then
    interval=5
  else
    interval=10
  fi

  [ "$interval" -le "$max_sleep" ] || interval="$max_sleep"

  case "$remaining" in
    *[!0-9]*|"") ;;
    0) interval=0 ;;
    *)
      if [ "$interval" -gt "$remaining" ]; then
        interval="$remaining"
      fi
      ;;
  esac

  printf '%s\n' "$interval"
}

oms_poll_log_next() {
  local label="$1"
  local elapsed="$2"
  local interval="$3"
  local remaining="${4:-}"
  local suffix=""

  [ "${OMS_POLL_VERBOSE:-0}" = "1" ] || return 0
  [ -n "$label" ] || label="wait"
  case "$remaining" in
    *[!0-9]*|"") ;;
    *) suffix=" remaining=${remaining}s" ;;
  esac
  printf 'oh-my-setting poll: %s elapsed=%ss next=%ss%s\n' "$label" "$elapsed" "$interval" "$suffix" >&2
}

oms_poll_sleep_labeled() {
  local label="$1"
  local elapsed="${2:-0}"
  local remaining="${3:-}"
  local interval

  interval="$(oms_poll_interval_seconds "$elapsed" "$remaining")"
  oms_poll_log_next "$label" "$elapsed" "$interval" "$remaining"
  [ "$interval" -gt 0 ] || return 0
  sleep "$interval"
}

oms_poll_sleep() {
  oms_poll_sleep_labeled wait "${1:-0}" "${2:-}"
}
