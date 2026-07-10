#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SUITE="${OMS_SMOKE_SUITE:-$ROOT/tests/scripts-smoke.sh}"
MODE="run"
SHARD=1
TOTAL=1
JOBS=""

usage() {
  cat <<'EOF'
Usage: run-smoke-shard.sh [--list] [--shard I/N] [--jobs N]

Run every test_* function defined in scripts-smoke.sh. --shard selects one
deterministic, 1-based round-robin partition. --jobs runs N shards in parallel.
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
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

for value in "$SHARD" "$TOTAL"; do
  case "$value" in *[!0-9]*|"") fail "shard values must be positive integers" ;; esac
done
[ "$TOTAL" -gt 0 ] || fail "shard total must be positive"
[ "$SHARD" -gt 0 ] && [ "$SHARD" -le "$TOTAL" ] || fail "shard index must be in 1..N"

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
  }
  trap cleanup_workers EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
  set -m
  i=1
  while [ "$i" -le "$JOBS" ]; do
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
awk -v shard="$SHARD" -v total="$TOTAL" '
  /^test_[[:alnum:]_]+\(\) \{/ {
    name=$1
    sub(/\(\)$/, "", name)
    names[++count]=name
  }
  $0 == "# SMOKE_TEST_CALLS_BEGIN" {
    print
    for (i=1; i<=count; i++) {
      if ((i - 1) % total == shard - 1) print names[i]
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
' "$SUITE" | OMS_SMOKE_RUNNER_ACTIVE=1 OMS_TEST_ROOT="$ROOT" bash
