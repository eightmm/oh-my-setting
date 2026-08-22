#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SUITE="${OMS_SMOKE_SUITE:-$ROOT/tests/scripts-smoke.sh}"
MODE="run"
SHARD=1
TOTAL=1
JOBS=""
ONLY=""
TIMINGS="${OMS_SMOKE_TIMINGS:-0}"
TIMING_LIMIT="${OMS_SMOKE_TIMING_LIMIT:-10}"
# Runner controls are not product/test inputs. Keep them out of the suite just
# as check.sh keeps its own mode flags out of descendants.
unset OMS_SMOKE_TIMINGS OMS_SMOKE_TIMING_LIMIT

usage() {
  cat <<'EOF'
Usage: run-smoke-shard.sh [--list] [--shard I/N] [--jobs N] [--only NAME]

Run every test_* function defined in scripts-smoke.sh. --shard selects one
deterministic, 1-based round-robin partition. --jobs runs N shards in parallel.
--only NAME runs just that test (repeatable); it is the focused-verification
path between edits, so a change to one behaviour does not cost the whole suite.
Names come from --list, and an unknown one is refused rather than silently
running nothing.

A passing run prints one line with its duration. Set OMS_VERBOSE=1 for the full
test output; a failing run always prints everything. OMS_SMOKE_TIMINGS=1 prints
the slowest tests, bounded by OMS_SMOKE_TIMING_LIMIT (default 10).
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --list) MODE="list"; shift ;;
    --shard)
      [ "$#" -ge 2 ] || fail "--shard requires I/N"
      case "$2" in
        */*) SHARD="${2%/*}"; TOTAL="${2#*/}" ;;
        *) fail "--shard requires I/N" ;;
      esac
      shift 2
      ;;
    --jobs)
      [ "$#" -ge 2 ] || fail "--jobs requires N"
      JOBS="$2"
      shift 2
      ;;
    --only)
      [ "$#" -ge 2 ] || fail "--only requires a test name"
      case "$2" in
        test_[A-Za-z0-9_]*) ;;
        *) fail "--only takes a test_* function name (see --list): $2" ;;
      esac
      ONLY="${ONLY:+$ONLY }$2"
      shift 2
      ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

RUN_SCRIPT="$(mktemp "${TMPDIR:-/tmp}/oms-smoke-suite.XXXXXX")"
trap 'rm -f "$RUN_SCRIPT"' EXIT

for value in "$SHARD" "$TOTAL"; do
  case "$value" in *[!0-9]*|"") fail "shard values must be positive integers" ;; esac
done
[ "$TOTAL" -gt 0 ] || fail "shard total must be positive"
[ "$SHARD" -gt 0 ] && [ "$SHARD" -le "$TOTAL" ] || fail "shard index must be in 1..N"

if [ -n "$ONLY" ]; then
  [ "$MODE" = "run" ] || fail "--only cannot be combined with --list"
  [ -z "$JOBS" ] || fail "--only cannot be combined with --jobs"
  [ "$SHARD" -eq 1 ] && [ "$TOTAL" -eq 1 ] || fail "--only cannot be combined with --shard"
fi

if [ -n "$JOBS" ]; then
  [ "$MODE" = "run" ] || fail "--jobs cannot be combined with --list"
  [ "$SHARD" -eq 1 ] && [ "$TOTAL" -eq 1 ] || fail "--jobs cannot be combined with --shard"
  case "$JOBS" in *[!0-9]*|"") fail "--jobs requires a positive integer" ;; esac
  [ "$JOBS" -gt 0 ] || fail "--jobs requires a positive integer"

  LOG_DIR="$(mktemp -d "${TMPDIR:-/tmp}/oms-smoke-shards.XXXXXX")"
  PIDS=""
  cleanup_workers() {
    local pid
    trap - EXIT HUP INT TERM
    for pid in $PIDS; do
      kill -TERM -- "-$pid" >/dev/null 2>&1 || kill -TERM "$pid" >/dev/null 2>&1 || true
    done
    sleep 1
    for pid in $PIDS; do
      kill -KILL -- "-$pid" >/dev/null 2>&1 || kill -KILL "$pid" >/dev/null 2>&1 || true
    done
    for pid in $PIDS; do
      wait "$pid" 2>/dev/null || true
    done
    rm -rf "$LOG_DIR"
    rm -f "$RUN_SCRIPT"
  }
  trap cleanup_workers EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
  set -m
  i=1
  while [ "$i" -le "$JOBS" ]; do
    OMS_SMOKE_TIMINGS="$TIMINGS" OMS_SMOKE_TIMING_LIMIT="$TIMING_LIMIT" \
      "$0" --shard "$i/$JOBS" > "$LOG_DIR/$i.log" 2>&1 &
    PIDS="$PIDS $!"
    i=$((i + 1))
  done
  set +m
  status=0
  i=1
  for pid in $PIDS; do
    if ! wait "$pid"; then
      status=1
    fi
    echo "== smoke shard $i/$JOBS =="
    cat "$LOG_DIR/$i.log"
    i=$((i + 1))
  done
  rm -rf "$LOG_DIR"
  rm -f "$RUN_SCRIPT"
  trap - EXIT HUP INT TERM
  exit "$status"
fi

manifest() {
  awk '
    /^test_[[:alnum:]_]+\(\) \{/ {
      name=$1
      sub(/\(\)$/, "", name)
      print name
    }
  ' "$SUITE"
}

noncanonical="$(awk '
  /^[[:space:]]*(function[[:space:]]+)?test_[[:alnum:]_]+[[:space:]]*\(\)[[:space:]]*\{/ &&
    $0 !~ /^test_[[:alnum:]_]+\(\) \{/ { print NR ":" $0 }
  /^[[:space:]]*function[[:space:]]+test_[[:alnum:]_]+[[:space:]]*\{/ {
    print NR ":" $0
  }
' "$SUITE")"
[ -z "$noncanonical" ] || fail "noncanonical smoke test definition (use test_name() {): $noncanonical"
[ "$(grep -Fxc '# SMOKE_TEST_CALLS_BEGIN' "$SUITE")" = "1" ] ||
  fail "smoke suite must contain exactly one # SMOKE_TEST_CALLS_BEGIN sentinel"
[ "$(grep -Fxc '# SMOKE_TEST_CALLS_END' "$SUITE")" = "1" ] ||
  fail "smoke suite must contain exactly one # SMOKE_TEST_CALLS_END sentinel"

duplicates="$(manifest | LC_ALL=C sort | uniq -d)"
[ -z "$duplicates" ] || fail "duplicate smoke test definition: $duplicates"

if [ "$MODE" = "list" ]; then
  manifest | awk -v shard="$SHARD" -v total="$TOTAL" '(NR - 1) % total == shard - 1'
  exit 0
fi

# Rewrite the explicitly marked legacy call tail in memory and derive calls
# from definitions, so a newly added test can never be silently omitted.
awk -v shard="$SHARD" -v total="$TOTAL" -v timings="$TIMINGS" -v only=" $ONLY " '
  /^test_[[:alnum:]_]+\(\) \{/ {
    name=$1
    sub(/\(\)$/, "", name)
    names[++count]=name
  }
  $0 == "# SMOKE_TEST_CALLS_BEGIN" {
    print
    if (timings == "1") {
      print "oms_smoke_run_test() {"
      print "  local oms_smoke_name=\"$1\""
      print "  local oms_smoke_started=\"${SECONDS:-0}\""
      print "  local oms_smoke_elapsed"
      print "  \"$oms_smoke_name\""
      print "  oms_smoke_elapsed=$((SECONDS - oms_smoke_started))"
      print "  printf \047%s\\t%s\\n\047 \"$oms_smoke_elapsed\" \"$oms_smoke_name\" >> \"$OMS_SMOKE_TIMING_FILE\""
      print "}"
    }
    for (i=1; i<=count; i++) {
      if (only != "  ") {
        if (index(only, " " names[i] " ") == 0) continue
      } else if ((i - 1) % total != shard - 1) {
        continue
      }
      if (timings == "1") print "oms_smoke_run_test " names[i]
      else print names[i]
    }
    emitted=1
    tail=1
    next
  }
  $0 == "# SMOKE_TEST_CALLS_END" {
    print
    tail=0
    next
  }
  tail {
    next
  }
  { print }
  END {
    if (!emitted) exit 3
  }
' "$SUITE" > "$RUN_SCRIPT"

# Passing tests say nothing but "ok": the per-test chatter is only evidence for
# a failure, and printing thousands of lines on every green run is context an
# agent has to read and pay for. A failure prints everything.
if [ -n "$ONLY" ]; then
  # An unknown name would otherwise run zero tests and still print ok, which is
  # the one answer a focused verification must never give.
  # Matched in-shell rather than through `manifest | grep -q`: grep exits on
  # the first match, awk takes SIGPIPE, and pipefail then reports the whole
  # pipeline as failed -- so the found case and the missing case both looked
  # like "unknown test".
  only_manifest="$(manifest)"
  for only_name in $ONLY; do
    case "
$only_manifest
" in
      *"
$only_name
"*) ;;
      *) fail "unknown test: $only_name (see --list)" ;;
    esac
  done
  run_count="$(printf '%s\n' $ONLY | grep -c .)"
else
  run_count="$(manifest | awk -v shard="$SHARD" -v total="$TOTAL" '(NR - 1) % total == shard - 1' | grep -c .)"
fi
run_log="$(mktemp "${TMPDIR:-/tmp}/oms-smoke-run.XXXXXX")"
timing_file=""
if [ "$TIMINGS" = "1" ]; then
  case "$TIMING_LIMIT" in
    *[!0-9]*|"") fail "OMS_SMOKE_TIMING_LIMIT requires a positive integer" ;;
  esac
  [ "$TIMING_LIMIT" -gt 0 ] || fail "OMS_SMOKE_TIMING_LIMIT requires a positive integer"
  timing_file="$(mktemp "${TMPDIR:-/tmp}/oms-smoke-timing.XXXXXX")"
fi
run_started="$(date +%s)"
run_status=0
OMS_SMOKE_RUNNER_ACTIVE=1 OMS_TEST_ROOT="$ROOT" \
  OMS_SMOKE_TIMING_FILE="$timing_file" bash "$RUN_SCRIPT" > "$run_log" 2>&1 || run_status=$?
run_elapsed=$(( $(date +%s) - run_started ))
if [ "$run_status" -ne 0 ] || [ "${OMS_VERBOSE:-0}" = "1" ]; then
  cat "$run_log"
fi
if [ -n "$timing_file" ] && [ -s "$timing_file" ]; then
  LC_ALL=C sort -rn "$timing_file" | sed -n "1,${TIMING_LIMIT}p" |
    while IFS="$(printf '\t')" read -r elapsed name; do
      printf 'smoke-timing: %ss %s\n' "$elapsed" "$name"
    done
fi
rm -f "$run_log" "$timing_file" "$RUN_SCRIPT"
if [ "$run_status" -eq 0 ]; then
  if [ "$TOTAL" -eq 1 ]; then
    echo "scripts-smoke: ok ($run_count tests, duration=${run_elapsed}s)"
  else
    echo "scripts-smoke: ok (shard $SHARD/$TOTAL, $run_count tests, duration=${run_elapsed}s)"
  fi
fi
exit "$run_status"
