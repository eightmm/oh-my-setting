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
deterministic, 1-based partition balanced by cost: a "# smoke-weight: SECONDS"
line directly above a definition records its measured cost, weighted tests go
heaviest first to the lightest shard, and the rest (2s each) fill the
lightest shard in definition order. --jobs runs N shards in parallel.
--only NAME runs just that test (repeatable); it is the focused-verification
path between edits, so a change to one behaviour does not cost the whole suite.
Combine --only with --jobs or --shard to partition just the selected tests.
Names come from --list, and an unknown one is refused rather than silently
running nothing.

A passing run prints one line with its duration. Set OMS_VERBOSE=1 for the full
test output; a failing run always prints everything. OMS_SMOKE_TIMINGS=1 prints
the slowest tests, bounded by OMS_SMOKE_TIMING_LIMIT (default 10).
When a check stage reaches its deadline, the running test is marked INTERRUPTED.
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
fi
if [ -n "$JOBS" ]; then
  [ "$MODE" = "run" ] || fail "--jobs cannot be combined with --list"
  [ "$SHARD" -eq 1 ] && [ "$TOTAL" -eq 1 ] || fail "--jobs cannot be combined with --shard"
  case "$JOBS" in *[!0-9]*|"") fail "--jobs requires a positive integer" ;; esac
  [ "$JOBS" -gt 0 ] || fail "--jobs requires a positive integer"
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
  pending_weight && $0 !~ /^test_[[:alnum:]_]+\(\) \{/ {
    print "smoke-weight line " pending_weight " is not directly above a test definition"
  }
  { pending_weight = 0 }
  /^# smoke-weight: [0-9]+$/ { pending_weight = NR }
  /^[[:space:]]*(function[[:space:]]+)?test_[[:alnum:]_]+[[:space:]]*\(\)[[:space:]]*\{/ &&
    $0 !~ /^test_[[:alnum:]_]+\(\) \{/ { print "noncanonical smoke test definition (use test_name() {): " NR ":" $0 }
  /^[[:space:]]*function[[:space:]]+test_[[:alnum:]_]+[[:space:]]*\{/ {
    print "noncanonical smoke test definition (use test_name() {): " NR ":" $0
  }
  END {
    if (pending_weight) print "smoke-weight line " pending_weight " is not directly above a test definition"
  }
' "$SUITE")"
[ -z "$noncanonical" ] || fail "$noncanonical"
[ "$(grep -Fxc '# SMOKE_TEST_CALLS_BEGIN' "$SUITE")" = "1" ] ||
  fail "smoke suite must contain exactly one # SMOKE_TEST_CALLS_BEGIN sentinel"
[ "$(grep -Fxc '# SMOKE_TEST_CALLS_END' "$SUITE")" = "1" ] ||
  fail "smoke suite must contain exactly one # SMOKE_TEST_CALLS_END sentinel"

duplicates="$(manifest | LC_ALL=C sort | uniq -d)"
[ -z "$duplicates" ] || fail "duplicate smoke test definition: $duplicates"

# One assignment serves --list, the worker count and the rewritten suite, so
# they cannot disagree. A single test dominated one CI shard (an agent-plan
# lifecycle test at 171-461s of a 255-828s shard), which a positional
# round-robin cannot see; the weights come from CI's OMS_SMOKE_TIMINGS lines.
shard_members() {  # the selected tests of SHARD/TOTAL, in definition order
  awk -v shard="$SHARD" -v total="$TOTAL" -v only=" $ONLY " '
    function place(i, cost,   s, pick) {
      pick = 1
      for (s = 2; s <= total; s++) if (load[s] < load[pick]) pick = s
      owner[i] = pick
      load[pick] += cost
    }
    /^# smoke-weight: [0-9]+$/ { pending = $3 + 0; next }
    /^test_[[:alnum:]_]+\(\) \{/ {
      name = $1
      sub(/\(\)$/, "", name)
      cost = pending
      pending = 0
      if (only != "  " && index(only, " " name " ") == 0) next
      names[++count] = name
      weight[count] = cost
      next
    }
    { pending = 0 }
    END {
      for (;;) {
        best = 0
        for (i = 1; i <= count; i++)
          if (weight[i] > 0 && !(i in owner) && (best == 0 || weight[i] > weight[best])) best = i
        if (best == 0) break
        place(best, weight[best])
      }
      for (i = 1; i <= count; i++) if (!(i in owner)) place(i, 2)
      for (i = 1; i <= count; i++) if (owner[i] == shard) print names[i]
    }
  ' "$SUITE"
}

if [ "$MODE" = "list" ]; then
  shard_members
  exit 0
fi

# Validate the complete selection before launching any worker. Match against
# definitions (not call sites); repeated selectors still execute only once.
all_manifest="$(manifest)"
[ -n "$all_manifest" ] || fail "no smoke test definitions"
for only_name in $ONLY; do
  case "
$all_manifest
" in
    *"
$only_name
"*) ;;
    *) fail "unknown test: $only_name (see --list)" ;;
  esac
done
members="$(shard_members)"
run_count="$(printf '%s' "$members" | awk 'END { print NR }')"
if [ "$run_count" -eq 0 ]; then
  echo "scripts-smoke: skipped (shard $SHARD/$TOTAL, no selected tests)"
  exit 0
fi

if [ -n "$JOBS" ]; then
  [ "$JOBS" -le "$run_count" ] || JOBS="$run_count"
  selection_args=()
  for only_name in $ONLY; do selection_args+=(--only "$only_name"); done

  LOG_DIR="$(mktemp -d "${TMPDIR:-/tmp}/oms-smoke-shards.XXXXXX")"
  PIDS=""
  # Shards are signalled only as the process groups set -m made them lead.
  # A running shard's group is ours. A finished shard's group is ours only
  # while it still has members and no process holds the leader's PID: POSIX
  # never reuses a PID that still names a process group, so leftovers of a
  # finished shard are reached and a reused PID (another gate or land job)
  # is not.
  cleanup_workers() {
    local pid running signalled=""
    trap - EXIT HUP INT TERM
    running=" $(jobs -pr | tr '\n' ' ') "
    for pid in $PIDS; do
      case "$running" in
        *" $pid "*) ;;
        *) ! kill -0 "$pid" >/dev/null 2>&1 && kill -0 -- "-$pid" >/dev/null 2>&1 || continue ;;
      esac
      kill -TERM -- "-$pid" >/dev/null 2>&1 && signalled="$signalled $pid"
    done
    sleep 1
    # A group with a live member keeps its ID reserved, so a shard signalled a
    # second ago may still be killed even if its leader already exited.
    for pid in $signalled; do
      kill -0 -- "-$pid" >/dev/null 2>&1 && { kill -KILL -- "-$pid" >/dev/null 2>&1 || true; }
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
      "$0" --shard "$i/$JOBS" ${selection_args[@]+"${selection_args[@]}"} > "$LOG_DIR/$i.log" 2>&1 &
    PIDS="$PIDS $!"
    i=$((i + 1))
  done
  set +m
  status=0
  i=1
  tries=0
  for pid in $PIDS; do
    if ! wait "$pid"; then
      status=1
    fi
    # Leftovers of a reaped shard die inside this step, while the group is
    # still verified ours (see cleanup_workers); the PID is never kept.
    if ! kill -0 "$pid" >/dev/null 2>&1 && kill -0 -- "-$pid" >/dev/null 2>&1; then
      kill -TERM -- "-$pid" >/dev/null 2>&1 || true
      tries=0
      while [ "$tries" -lt 10 ] && ! kill -0 "$pid" >/dev/null 2>&1 &&
          kill -0 -- "-$pid" >/dev/null 2>&1; do
        sleep 0.1
        tries=$((tries + 1))
      done
      if ! kill -0 "$pid" >/dev/null 2>&1 && kill -0 -- "-$pid" >/dev/null 2>&1; then
        kill -KILL -- "-$pid" >/dev/null 2>&1 || true
      fi
    fi
    # A reaped shard leaves the cleanup set before its PID can be reused.
    PIDS="$(printf ' %s ' $PIDS | sed "s/ $pid / /")"
    echo "== smoke shard $i/$JOBS =="
    cat "$LOG_DIR/$i.log"
    i=$((i + 1))
  done
  rm -rf "$LOG_DIR"
  rm -f "$RUN_SCRIPT"
  trap - EXIT HUP INT TERM
  exit "$status"
fi

# Rewrite the explicitly marked legacy call tail in memory and derive calls
# from definitions, so a newly added test can never be silently omitted.
awk -v timings="$TIMINGS" -v members=" $(printf '%s' "$members" | tr '\n' ' ') " '
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
      print "  local oms_smoke_started=\"$(date +%s)\""
      print "  local oms_smoke_elapsed"
      print "  local oms_smoke_status"
      print "  \"$oms_smoke_name\""
      print "  oms_smoke_status=$?"
      print "  oms_smoke_elapsed=$(($(date +%s) - oms_smoke_started))"
      print "  printf \047%s\\t%s\\n\047 \"$oms_smoke_elapsed\" \"$oms_smoke_name\" >> \"$OMS_SMOKE_TIMING_FILE\""
      print "  return \"$oms_smoke_status\""
      print "}"
    }
    for (i=1; i<=count; i++) {
      if (index(members, " " names[i] " ") == 0) continue
      # The marker names the test a failure belongs to.
      print "if [ -n \"${OMS_CHECK_SMOKE_PROGRESS:-}\" ]; then printf \047%s\\n\047 " names[i] " >> \"$OMS_CHECK_SMOKE_PROGRESS\" || true; fi"
      print "printf \047smoke-test: %s\\n\047 " names[i]
      if (timings == "1") print "oms_smoke_run_test " names[i]
      else print names[i]
      print "oms_smoke_status=$?"
      print "if [ -n \"${OMS_CHECK_SMOKE_PROGRESS:-}\" ]; then printf \047\\n\047 >> \"$OMS_CHECK_SMOKE_PROGRESS\" || true; fi"
      print "(exit \"$oms_smoke_status\")"
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
run_log="$(mktemp "${TMPDIR:-/tmp}/oms-smoke-run.XXXXXX")"
timing_file=""
progress_file=""
trap 'rm -f "$RUN_SCRIPT" "$run_log" ${timing_file:+"$timing_file"} ${progress_file:+"$progress_file"}' EXIT
if [ -n "${OMS_CHECK_DEADLINE_MARKER:-}" ]; then
  progress_file="$(mktemp "${OMS_CHECK_DEADLINE_MARKER}.progress.XXXXXX" 2>/dev/null)" || progress_file=""
  if [ -n "$progress_file" ]; then
    printf '%s\n' "$(basename "$SUITE" .sh)" > "$progress_file" || progress_file=""
  fi
fi
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
  OMS_SMOKE_TIMING_FILE="$timing_file" OMS_CHECK_SMOKE_PROGRESS="$progress_file" bash "$RUN_SCRIPT" > "$run_log" 2>&1 || run_status=$?
active_test=""
if [ -n "$progress_file" ] && [ -f "$progress_file" ]; then
  active_test="$(tail -n 1 "$progress_file")"
fi
[ -z "$progress_file" ] || : > "$progress_file" || true
run_elapsed=$(( $(date +%s) - run_started ))
if [ "$run_status" -ne 0 ] || [ "${OMS_VERBOSE:-0}" = "1" ]; then
  cat "$run_log"
fi
if [ "$run_status" -ne 0 ]; then
  # Same shape as a check stage, so the land fail ledger records the test.
  failed_test="$(sed -n 's/^smoke-test: //p' "$run_log" | tail -n 1)"
  if [ -n "$failed_test" ]; then
    if [ -n "${OMS_CHECK_DEADLINE_MARKER:-}" ] && [ -f "$OMS_CHECK_DEADLINE_MARKER" ]; then
      [ "$active_test" != "$failed_test" ] || echo "check: $(basename "$SUITE" .sh).$failed_test INTERRUPTED (stage deadline)"
    else
      echo "check: $(basename "$SUITE" .sh).$failed_test FAILED"
    fi
  fi
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
