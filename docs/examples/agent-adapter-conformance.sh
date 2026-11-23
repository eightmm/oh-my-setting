#!/usr/bin/env bash
set -euo pipefail

# Reference conformance check for the one-shot OMS custom adapter contract
# (custom-skills/oms-agent-harness/references/provider-routing.md, "Custom
# adapters"). It runs one adapter executable directly; it does not call OMS.
#
# Default mode runs only the name, executable and bounded probe checks: no
# inference request, login, install or configuration change. `--run` invokes
# the adapter's real `run` verb in a throwaway repository and may spend the
# provider's quota; `--write` adds a write call. A pass here is fixture-level
# evidence for the checks listed, never proof of sandboxing, live provider
# compatibility or OMS admission.

usage() {
  cat <<'EOF'
usage: agent-adapter-conformance.sh [--run] [--write] [--probe-bound SECONDS]
                                    [--run-bound SECONDS] ADAPTER
       agent-adapter-conformance.sh --self-test

  ADAPTER              Path to an oms-agent-adapter-ID executable.
  --run                Also call `run --access read` (may spend quota).
  --write              With --run, also call `run --access write`.
  --probe-bound N      Seconds allowed for --version and --help (default 10).
  --run-bound N        Seconds allowed for each run call (default 300).
  --self-test          Check this script against stub adapters.

Each check prints `ok`, `fail` or `unknown` with its name. Exit status is 1
when any check fails, 2 for a usage error.
EOF
}

RUN=0
WRITE=0
SELF_TEST=0
SELF_DIR=""
PROBE_BOUND=10
RUN_BOUND=300
ADAPTER=""
bound=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --run) RUN=1 ;;
    --write) WRITE=1 ;;
    --self-test) SELF_TEST=1 ;;
    --probe-bound | --run-bound)
      [ "$#" -ge 2 ] || { usage >&2; exit 2; }
      case "$2" in
        '' | *[!0-9]*) echo "error: $1 needs a positive decimal integer" >&2; exit 2 ;;
      esac
      # Decimal, not Bash octal: 08 is 8. Cap at 86400 s (and 6 digits) first.
      bound="${2#"${2%%[!0]*}"}"
      if [ -z "$bound" ] || [ "${#bound}" -gt 5 ] || [ "$bound" -gt 86400 ]; then
        echo "error: $1 must be 1..86400" >&2
        exit 2
      fi
      if [ "$1" = --probe-bound ]; then PROBE_BOUND="$bound"; else RUN_BOUND="$bound"; fi
      shift
      ;;
    -h | --help) usage; exit 0 ;;
    -*) usage >&2; exit 2 ;;
    *)
      [ -z "$ADAPTER" ] || { usage >&2; exit 2; }
      ADAPTER="$1"
      ;;
  esac
  shift
done
if [ "$WRITE" = 1 ] && [ "$RUN" = 0 ]; then
  echo "error: --write requires --run" >&2
  exit 2
fi

FAILED=0
report() { # report STATUS NAME DETAIL
  printf '%-8s %-20s %s\n' "$1" "$2" "$3"
  if [ "$1" = fail ]; then FAILED=1; fi
}

# run_bounded SECONDS STDOUT_FILE STDERR_FILE COMMAND...
# Sets RUN_STATUS (124 on timeout) and RUN_LEFTOVER (1 when a process of the
# command's own process group outlives it). A child that starts a new session
# leaves that group and is not seen.
RUN_STATUS=0
RUN_LEFTOVER=0
RUN_DIR="" # when set, the command starts with this working directory
run_bounded() {
  local bound="$1" out="$2" err="$3" pid ticks=0 limit
  shift 3
  limit=$((bound * 10))
  RUN_STATUS=0
  RUN_LEFTOVER=0
  set -m
  (
    if [ -n "$RUN_DIR" ]; then cd "$RUN_DIR" || exit 126; fi
    exec "$@"
  ) < /dev/null > "$out" 2> "$err" &
  pid=$!
  set +m
  while kill -0 "$pid" 2> /dev/null; do
    if [ "$ticks" -ge "$limit" ]; then
      kill -TERM -- "-$pid" 2> /dev/null || true
      sleep 1
      kill -KILL -- "-$pid" 2> /dev/null || true
      wait "$pid" 2> /dev/null || true
      RUN_STATUS=124
      return 0
    fi
    sleep 0.1
    ticks=$((ticks + 1))
  done
  wait "$pid" 2> /dev/null || RUN_STATUS=$?
  if kill -0 -- "-$pid" 2> /dev/null; then
    RUN_LEFTOVER=1
    kill -KILL -- "-$pid" 2> /dev/null || true
  fi
}

check_name() {
  local base id
  base="$(basename "$ADAPTER")"
  id="${base#oms-agent-adapter-}"
  if [ "$id" = "$base" ] || [ -z "$id" ] || [ "${#id}" -gt 64 ]; then
    report fail name "expected oms-agent-adapter-ID, got $base"
    return
  fi
  case "$id" in
    *..* | [!a-z0-9]* | *[!a-z0-9._-]*)
      report fail name "ID must be lowercase [a-z0-9._-], start alphanumeric, no '..'"
      ;;
    *) report ok name "$id" ;;
  esac
}

check_probe() { # check_probe FLAG
  local flag="$1" name="${1#--}-probe"
  run_bounded "$PROBE_BOUND" "$WORK/probe.out" "$WORK/probe.err" "$ADAPTER" "$flag"
  if [ "$RUN_STATUS" = 124 ]; then
    report fail "$name" "no exit within ${PROBE_BOUND}s"
  elif [ "$RUN_STATUS" != 0 ]; then
    report fail "$name" "exit $RUN_STATUS"
  else
    report ok "$name" "exit 0 within ${PROBE_BOUND}s"
  fi
}

new_repo() { # new_repo DIRECTORY
  git init -q "$1"
  git -C "$1" config user.name conformance
  git -C "$1" config user.email conformance@example.invalid
  printf 'fixture\n' > "$1/README.md"
  git -C "$1" add README.md
  git -C "$1" commit -q -m fixture
}

check_run() { # check_run read|write
  local access="$1" repo="$WORK/repo-$1" prompt="$WORK/prompt-$1" base exe
  new_repo "$repo"
  base="$(git -C "$repo" rev-parse HEAD)"
  printf 'Reply with the single word: ready\n' > "$prompt"
  cp "$prompt" "$prompt.orig"
  # OMS starts the adapter inside the workdir; keep the pathname as given, made
  # absolute so the change of directory cannot break a relative one.
  case "$ADAPTER" in /*) exe="$ADAPTER" ;; *) exe="$PWD/$ADAPTER" ;; esac
  RUN_DIR="$repo"
  run_bounded "$RUN_BOUND" "$WORK/run.out" "$WORK/run.err" \
    "$exe" run --access "$access" --workdir "$repo" --prompt-file "$prompt"
  RUN_DIR=""
  if [ "$RUN_STATUS" = 124 ]; then
    report fail "$access-exit" "no exit within ${RUN_BOUND}s"
    return
  fi
  if [ "$RUN_STATUS" != 0 ]; then
    report fail "$access-exit" "exit $RUN_STATUS (stderr: $(head -c 200 "$WORK/run.err" | tr '\n' ' '))"
    return
  fi
  report ok "$access-exit" "exit 0"
  if [ -s "$WORK/run.out" ]; then
    report ok "$access-answer" "final answer on stdout"
  else
    report fail "$access-answer" "stdout is empty"
  fi
  if [ "$RUN_LEFTOVER" = 1 ]; then
    report fail "$access-background" "a process of the adapter's group outlived the call"
  else
    report ok "$access-background" "no process left in the adapter's group"
  fi
  if cmp -s "$prompt" "$prompt.orig"; then
    report ok "$access-prompt" "prompt file unchanged"
  else
    report fail "$access-prompt" "prompt file was modified"
  fi
  if [ "$(git -C "$repo" rev-parse HEAD)" = "$base" ]; then
    report ok "$access-commit" "no commit"
  else
    report fail "$access-commit" "the adapter committed"
  fi
  if [ -e "$repo/.oms" ]; then
    report fail "$access-state" "the adapter created .oms"
  else
    report ok "$access-state" ".oms untouched"
  fi
  if [ "$access" = read ]; then
    if [ -z "$(git -C "$repo" status --porcelain)" ]; then
      report ok read-clean "worktree unchanged"
    else
      report fail read-clean "a read call changed the worktree"
    fi
  fi
}

self_test() {
  local dir self status out run_args
  self="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/$(basename "${BASH_SOURCE[0]}")"
  SELF_DIR="$(mktemp -d "${TMPDIR:-/tmp}/oms-adapter-conformance.XXXXXX")"
  trap 'rm -rf "$SELF_DIR"' EXIT HUP INT TERM
  dir="$SELF_DIR"
  stub() { # stub NAME BODY
    printf '%s\n' '#!/usr/bin/env bash' 'set -eu' "$2" > "$dir/$1"
    chmod +x "$dir/$1"
  }
  # The stub bodies are literal script text, expanded only when a stub runs.
  # shellcheck disable=SC2016
  run_args='workdir=.; while [ "$#" -gt 0 ]; do case "$1" in --workdir) workdir="$2"; shift ;; esac; shift; done'
  stub oms-agent-adapter-good \
    "case \"\${1:-}\" in --version | --help) exit 0 ;; esac; $run_args; : \"\$workdir\"; echo ready"
  # shellcheck disable=SC2016
  stub oms-agent-adapter-cwd \
    "case \"\${1:-}\" in --version | --help) exit 0 ;; esac; $run_args; [ \"\$(pwd -P)\" = \"\$(cd \"\$workdir\" && pwd -P)\" ] || exit 7; echo ready"
  # shellcheck disable=SC2016
  stub oms-agent-adapter-slow \
    'case "${1:-}" in --version) sleep 30 ;; --help) exit 0 ;; esac'
  stub oms-agent-adapter-writer \
    "case \"\${1:-}\" in --version | --help) exit 0 ;; esac; $run_args; echo changed > \"\$workdir/extra\"; echo ready"
  stub oms-agent-adapter-daemon \
    "case \"\${1:-}\" in --version | --help) exit 0 ;; esac; $run_args; sleep 30 > /dev/null 2>&1 & echo ready"
  stub Bad_Name 'exit 0'

  expect() { # expect LABEL STATUS PATTERN -- ARGS...
    local label="$1" want="$2" pattern="$3"
    shift 4
    status=0
    out="$(bash "$self" "$@" 2>&1)" || status=$?
    if [ "$status" != "$want" ]; then
      echo "self-test FAIL: $label exited $status, want $want: $out" >&2
      exit 1
    fi
    if ! printf '%s\n' "$out" | grep -Eq "$pattern"; then
      echo "self-test FAIL: $label lacks /$pattern/: $out" >&2
      exit 1
    fi
  }
  expect good 0 '^ok +read-clean ' -- --run --write "$dir/oms-agent-adapter-good"
  expect cwd 0 '^ok +read-exit ' -- --run "$dir/oms-agent-adapter-cwd"
  (cd "$dir" && expect cwd-relative 0 '^ok +read-exit ' -- --run ./oms-agent-adapter-cwd)
  expect octal-bound 0 '^ok +version-probe ' -- --probe-bound 08 "$dir/oms-agent-adapter-good"
  expect zero-bound 2 'positive|1\.\.86400' -- --run-bound 00 "$dir/oms-agent-adapter-good"
  expect huge-bound 2 '1\.\.86400' -- --probe-bound 99999999999999999999 "$dir/oms-agent-adapter-good"
  expect slow 1 '^fail +version-probe ' -- --probe-bound 1 "$dir/oms-agent-adapter-slow"
  expect writer 1 '^fail +read-clean ' -- --run "$dir/oms-agent-adapter-writer"
  expect daemon 1 '^fail +read-background ' -- --run "$dir/oms-agent-adapter-daemon"
  expect name 1 '^fail +name ' -- "$dir/Bad_Name"
  echo "agent-adapter-conformance self-test: ok"
}

if [ "$SELF_TEST" = 1 ]; then
  self_test
  exit 0
fi
[ -n "$ADAPTER" ] || { usage >&2; exit 2; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/oms-adapter-check.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT HUP INT TERM

check_name
if [ -f "$ADAPTER" ] && [ -x "$ADAPTER" ]; then
  report ok executable "$ADAPTER"
else
  report fail executable "not an executable regular file: $ADAPTER"
  exit 1
fi
check_probe --version
check_probe --help
if [ "$RUN" = 1 ]; then
  check_run read
  if [ "$WRITE" = 1 ]; then check_run write; fi
else
  report unknown read-call "not run; pass --run to call the adapter's agent"
fi
report unknown exit-status "provider failure is not induced here"
report unknown isolation "network, credentials and writes outside the workdir are not observed"
exit "$FAILED"
