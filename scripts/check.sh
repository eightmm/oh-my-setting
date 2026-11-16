#!/usr/bin/env bash
[ "${BASH_VERSINFO[0]:-0}" -ge 5 ] || { echo "error: check.sh needs Bash 5 or newer (running ${BASH_VERSION:-unknown})" >&2; exit 1; }
set -euo pipefail
umask 022

# Repository verification gate: shellcheck lint, skill manifest validation,
# markdown reference checks, Rust migration inventory/tests, and pure-Bash E2E
# smoke suites. Pure Bash + Rust only (no Python required).

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"

unset GIT_DIR GIT_WORK_TREE GIT_COMMON_DIR GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_PREFIX
unset TMUX TMUX_PANE OMS_STATE_REPO OMS_PLAN_LEASE_ID OMS_TASK_ID OMS_ATTEMPT_ID

CHECK_ORIGINAL_LOCK_DIR="${OMS_LOCK_DIR:-}"
CHECK_RUNTIME="$(mktemp -d "${TMPDIR:-/tmp}/oms-check-runtime.XXXXXX")"
mkdir -p "$CHECK_RUNTIME/locks" "$CHECK_RUNTIME/config" "$CHECK_RUNTIME/cache" "$CHECK_RUNTIME/timings-dir"
export OMS_LOCK_DIR="$CHECK_RUNTIME/locks"
export XDG_CONFIG_HOME="$CHECK_RUNTIME/config"
export XDG_CACHE_HOME="$CHECK_RUNTIME/cache"
: > "$CHECK_RUNTIME/timings"

CHECK_STATE_GUARD_ACTIVE=0
CHECK_STAGE_PID=""
cleanup_check() {
  local rc=$?
  trap - EXIT HUP INT TERM
  if [ -n "${CHECK_STAGE_PID:-}" ]; then
    kill -TERM -- "-$CHECK_STAGE_PID" 2>/dev/null || kill -TERM "$CHECK_STAGE_PID" 2>/dev/null || true
    wait "$CHECK_STAGE_PID" 2>/dev/null || true
  fi
  if declare -F oms_check_maintenance_end >/dev/null; then
    oms_check_maintenance_end || true
  fi
  rm -rf "$CHECK_RUNTIME"
  exit "$rc"
}
trap cleanup_check EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

MODE=full
PARALLEL=0
SKIP_LINT=0
SMOKE_SHARD=""
QUICK_FROM=""
QUICK_TO=""
AFFECTED_MODE=""
AFFECTED_REASONS=""
AFFECTED_NATIVE=false
FOCUSED_LANE=""
FOCUSED_LANE_I=0
FOCUSED_LANE_N=0
FOCUSED_STAGE_INDEX=0
WITHOUT_OPERATOR=0
LIST_STAGES=0
PRINT_AFFECTED_MODE=0
CI_OUTPUT=""
CI_PARITY=0

usage() {
  cat <<'EOF'
usage: check.sh [MODE] [OPTIONS]

Runs the pure-Rust repository gate: lint (shellcheck, oms-migrate inventory,
skill manifest, doc references) then focused smoke suites and Rust tests.

  --no-lint             Skip lint only when equivalent checks ran separately.
  --lint-only           Run only lint stages.
  --parallel            Run lint, focused suites, and Rust tests concurrently.
  --focused-only        Run only the focused smoke suites.
  --focused-lane I/N    Run every Nth focused stage starting at Ith.
  --without-operator-tools
                        Accepted compatibility flag for lane partitioning.
  --operator-only       Legacy no-op (operator suites retired into Rust tests).
  --scripts-smoke-only  Legacy no-op (retired into Rust workspace tests).
  invariants            Run fast repository-wide pre-land invariants.
  --shard I/N           Accepted compatibility flag.
  --list-stages         Print selected stage names without running them.
  --quick               Run changed-file checks plus invariants.
  --affected            Plan and verify affected scope (--changed-from/--changed-to).
  --print-affected-mode Print affected or full after planning and exit.
  --ci-output FILE      Append mode=..., native=..., python=false, operator= to FILE.
  --ci-parity           Affected gate with provider CLIs hidden from PATH.
  --changed-from REF    Base tree for --quick or --affected.
  --changed-to REF      Target tree for --quick or --affected.
EOF
}

select_mode() {
  [ "$MODE" = full ] || { echo "error: mutually exclusive modes: $MODE and $1" >&2; exit 2; }
  MODE="$1"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --parallel) PARALLEL=1; shift ;;
    --no-lint) SKIP_LINT=1; shift ;;
    --lint-only) select_mode lint; shift ;;
    --focused-only) select_mode focused; shift ;;
    --scripts-smoke-only) select_mode scripts-smoke; shift ;;
    --operator-only) select_mode operator; shift ;;
    --without-operator-tools) WITHOUT_OPERATOR=1; shift ;;
    invariants) select_mode invariants; shift ;;
    --shard)
      [ "$#" -ge 2 ] || { echo "error: --shard requires I/N" >&2; exit 2; }
      SMOKE_SHARD="$2"; shift 2 ;;
    --focused-lane)
      [ "$#" -ge 2 ] || { echo "error: --focused-lane requires I/N" >&2; exit 2; }
      FOCUSED_LANE="$2"; shift 2 ;;
    --list-stages) LIST_STAGES=1; shift ;;
    --quick) select_mode quick; shift ;;
    --affected) select_mode affected; shift ;;
    --print-affected-mode) PRINT_AFFECTED_MODE=1; shift ;;
    --ci-output)
      [ "$#" -ge 2 ] && [ -n "$2" ] || { echo "error: --ci-output requires a file" >&2; exit 2; }
      CI_OUTPUT="$2"; shift 2 ;;
    --ci-parity) CI_PARITY=1; shift ;;
    --changed-from)
      [ "$#" -ge 2 ] || { echo "error: --changed-from requires a ref" >&2; exit 2; }
      QUICK_FROM="$2"; shift 2 ;;
    --changed-to)
      [ "$#" -ge 2 ] || { echo "error: --changed-to requires a ref" >&2; exit 2; }
      QUICK_TO="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
done

[ "$WITHOUT_OPERATOR" != 999 ] || :
[ -z "$SMOKE_SHARD" ] || :

if [ -n "${OMS_CHECK_JOBS:-}" ]; then
  case "$OMS_CHECK_JOBS" in 0|*[!0-9]*) echo 'error: OMS_CHECK_JOBS must be a positive integer' >&2; exit 2 ;; esac
  [ "$OMS_CHECK_JOBS" -gt 0 ] || exit 2
  export OMS_LINT_JOBS_MAX="${OMS_LINT_JOBS_MAX:-$OMS_CHECK_JOBS}"
fi

if [ -n "$FOCUSED_LANE" ]; then
  [ "$MODE" = focused ] || { echo "error: --focused-lane requires --focused-only" >&2; exit 2; }
  case "$FOCUSED_LANE" in
    [1-9]/[1-9]|[1-9]/1[0-6]|1[0-6]/1[0-6]) ;;
    *) echo "error: --focused-lane must be I/N with 1 <= I <= N <= 16" >&2; exit 2 ;;
  esac
  FOCUSED_LANE_I="${FOCUSED_LANE%%/*}"
  FOCUSED_LANE_N="${FOCUSED_LANE##*/}"
  [ "$FOCUSED_LANE_I" -le "$FOCUSED_LANE_N" ] || { echo "error: invalid --focused-lane" >&2; exit 2; }
fi

if [ "$MODE" = quick ] || [ "$MODE" = affected ]; then
  [ -n "$QUICK_FROM" ] && [ -n "$QUICK_TO" ] || { echo "error: --$MODE requires --changed-from and --changed-to" >&2; exit 2; }
fi

# Stage native payload once in parent before any parallel/focused checks run.
if [ "$LIST_STAGES" != 1 ]; then
  # shellcheck source=scripts/lib/check-maintenance.sh
  . "$ROOT/scripts/lib/check-maintenance.sh"
  OMS_LOCK_DIR="$CHECK_ORIGINAL_LOCK_DIR" oms_check_maintenance_begin "$ROOT"
  # shellcheck source=scripts/lib/platform.sh
  . "$ROOT/scripts/lib/platform.sh"
  if ! oms_native_resolve "$ROOT" >/dev/null 2>&1; then
    [ "${OMS_NATIVE_NO_AUTO_STAGE:-0}" = 1 ] || oms_native_stage "$ROOT" || true
  fi
fi

if [ "$PARALLEL" = 1 ]; then
  [ "$MODE" = full ] || { echo "error: --parallel requires the complete gate" >&2; exit 2; }
  if [ "$LIST_STAGES" != 1 ]; then
    # shellcheck source=scripts/lib/check-parallel.sh
    . "$ROOT/scripts/lib/check-parallel.sh"
    oms_check_parallel "$ROOT/scripts/check.sh" "$CHECK_RUNTIME/parallel" "$SKIP_LINT"
    if [ "$SKIP_LINT" = 1 ]; then
      echo "check: ok (parallel gate without lint)"
    else
      echo "check: ok (parallel complete gate)"
    fi
    exit 0
  fi
fi

RUN_LINT=0
RUN_FOCUSED=0
RUN_RUST=0
RUN_INVARIANTS=0
RUN_QUICK=0
RUN_AFFECTED=0
case "$MODE" in
  full) [ "$SKIP_LINT" = 1 ] || RUN_LINT=1; RUN_FOCUSED=1; RUN_RUST=1 ;;
  lint) RUN_LINT=1 ;;
  focused) RUN_FOCUSED=1 ;;
  scripts-smoke|operator) echo "ok: $MODE (covered natively in rust workspace tests)"; exit 0 ;;
  invariants) RUN_INVARIANTS=1 ;;
  quick) RUN_QUICK=1 ;;
  affected) RUN_AFFECTED=1 ;;
esac

SHELLCHECK="${OMS_SHELLCHECK_BIN:-shellcheck}"
lint_shell() {
  local jobs
  jobs="$(getconf _NPROCESSORS_ONLN 2>/dev/null || nproc 2>/dev/null || echo 2)"
  case "$jobs" in ''|*[!0-9]*) jobs=2 ;; esac
  [ "$jobs" -le "${OMS_LINT_JOBS_MAX:-4}" ] || jobs="${OMS_LINT_JOBS_MAX:-4}"
  [ "$jobs" -ge 1 ] || jobs=1
  ls -S -- "$@" | tr '\n' '\0' | xargs -0 -P "$jobs" -n 2 "$SHELLCHECK" -x -S warning
}
export -f lint_shell
export SHELLCHECK

if [ "$RUN_LINT" = 1 ] && ! command -v "$SHELLCHECK" >/dev/null 2>&1; then
  echo "FATAL: shellcheck is not installed" >&2
  exit 1
fi

# Snapshot top-level .oms entries so tests never leak writes into the checkout.
oms_state_fingerprint() {
  [ -d "$ROOT/.oms" ] || return 0
  local extra_excludes=()
  if [ -n "${OMS_PANEL_REPO:-}" ] || [ "${OMS_CHECK_ALLOW_LIVE_PANEL:-0}" = 1 ]; then
    extra_excludes+=(-not -path './hooks/panel-cache*' -not -path './panel-state*' -not -path './lifecycle/events.jsonl*')
  fi
  (
    cd "$ROOT/.oms"
    find . -maxdepth 3 -not -path './tick*' -not -path './locks*' "${extra_excludes[@]}" -exec ls -ld --full-time {} + 2>/dev/null | LC_ALL=C sort ||
      find . -maxdepth 3 -not -path './tick*' -not -path './locks*' "${extra_excludes[@]}" 2>/dev/null | LC_ALL=C sort
  )
}
STATE_BEFORE="$CHECK_RUNTIME/oms-state.before"
STATE_AFTER="$CHECK_RUNTIME/oms-state.after"
verify_oms_state() {
  [ "$CHECK_STATE_GUARD_ACTIVE" = 1 ] || return 0
  CHECK_STATE_GUARD_ACTIVE=0
  oms_state_fingerprint > "$STATE_AFTER" || return $?
  if ! cmp -s "$STATE_BEFORE" "$STATE_AFTER"; then
    echo "check: a suite wrote into this checkout's .oms state" >&2
    diff -u "$STATE_BEFORE" "$STATE_AFTER" | sed -n '1,30p' >&2 || true
    return 1
  fi
  return 0
}
oms_state_fingerprint > "$STATE_BEFORE"
CHECK_STATE_GUARD_ACTIVE=1

run_stage_with_deadline() {
  local wall="$1" rc=0 child timer=""
  shift
  case "$wall" in ''|0|*[!0-9]*) echo "error: invalid stage timeout: $wall" >&2; return 2 ;; esac
  set -m
  "$@" </dev/null &
  child=$!
  set +m
  (
    sleep "$wall"
    kill -TERM -- "-$child" 2>/dev/null || kill -TERM "$child" 2>/dev/null || true
    sleep 2
    kill -KILL -- "-$child" 2>/dev/null || kill -KILL "$child" 2>/dev/null || true
  ) >/dev/null 2>&1 &
  timer=$!
  wait "$child" || rc=$?
  kill "$timer" 2>/dev/null || true
  wait "$timer" 2>/dev/null || true
  return "$rc"
}

stage() {
  local name="$1" log rc=0 started elapsed deadline=1800
  shift
  if [ -n "$FOCUSED_LANE" ]; then
    FOCUSED_STAGE_INDEX=$((FOCUSED_STAGE_INDEX + 1))
    if [ $(( (FOCUSED_STAGE_INDEX - 1) % FOCUSED_LANE_N )) -ne $((FOCUSED_LANE_I - 1)) ]; then
      return 0
    fi
  fi
  if [ "$LIST_STAGES" = 1 ]; then
    echo "$name"
    return 0
  fi
  log="$(mktemp "${TMPDIR:-/tmp}/oms-check.XXXXXX")"
  started="$(date +%s)"
  run_stage_with_deadline "${OMS_CHECK_STAGE_TIMEOUT_SECONDS:-$deadline}" "$@" > "$log" 2>&1 &
  CHECK_STAGE_PID=$!
  wait "$CHECK_STAGE_PID" || rc=$?
  CHECK_STAGE_PID=""
  elapsed=$(( $(date +%s) - started ))
  if [ "${OMS_VERBOSE:-0}" = "1" ]; then
    cat "$log"; rm -f "$log"
  elif [ "$rc" -ne 0 ]; then
    cat "$log"; rm -f "$log"
  else
    rm -f "$log"
  fi
  if [ "$rc" -ne 0 ]; then
    echo "check: $name FAILED (${elapsed}s)" >&2
    exit "$rc"
  fi
  echo "ok: $name (${elapsed}s)"
}

is_provider_cli() {
  case " $CHECK_PROVIDER_CLIS " in *" $1 "*) return 0 ;; esac
  case "$1" in oms-agent-adapter-?*) return 0 ;; esac
  return 1
}
hide_provider_clis() {
  local rest="$PATH:" dir entry base hit shadow count=0 filtered=""
  CHECK_PROVIDER_CLIS="opencode opencode2 $(
    # shellcheck source=scripts/lib/provider-registry.sh
    . "$ROOT/scripts/lib/provider-registry.sh"
    for name in $(oms_provider_supported_names); do oms_provider_binary "$name"; done | tr '\n' ' '
  )"
  while [ -n "$rest" ]; do
    dir="${rest%%:*}"; rest="${rest#*:}"; hit=0
    if [ -n "$dir" ] && [ -d "$dir" ]; then
      for entry in "$dir"/*; do
        if is_provider_cli "${entry##*/}" && [ -e "$entry" ]; then hit=1; break; fi
      done
    fi
    if [ "$hit" = 1 ]; then
      count=$((count + 1)); shadow="$CHECK_RUNTIME/provider-less/$count"; mkdir -p "$shadow"
      dir="$(cd "$dir" && pwd -P)"
      for entry in "$dir"/*; do
        [ -e "$entry" ] || continue
        base="${entry##*/}"
        is_provider_cli "$base" || ln -s "$entry" "$shadow/$base"
      done
      dir="$shadow"
    fi
    [ -z "$dir" ] || filtered="${filtered:+$filtered:}$dir"
  done
  PATH="$filtered"; export PATH; hash -r
  echo "ci-parity: provider CLIs hidden from PATH ($count shadowed)"
}

prepare_affected_gate() {
  local changes="$CHECK_RUNTIME/affected-changes" path docs_only=1 scoped_only=1 reason
  # shellcheck source=scripts/lib/critical-paths.sh
  . "$ROOT/scripts/lib/critical-paths.sh"
  AFFECTED_NATIVE=false
  git diff --name-only --no-renames "$QUICK_FROM" "$QUICK_TO" -- > "$changes"
  while IFS= read -r path; do
    case "$path" in
      .github/workflows/*|install.sh|scripts/oms|scripts/lib/platform.sh|scripts/lib/*lock*|\
      Cargo.toml|Cargo.lock|rust-toolchain.toml|crates/*|tests/*install*|tests/*portability*)
        AFFECTED_NATIVE=true ;;
    esac
    case "$path" in
      README.md|README.ko.md|CHANGELOG.md|CONTRIBUTING.md|docs/*.md) ;;
      *) docs_only=0 ;;
    esac
    case "$path" in
      README.md|README.ko.md|CHANGELOG.md|CONTRIBUTING.md|docs/*.md|custom-skills/*.md|templates/*.md|roles/*.md) ;;
      *) scoped_only=0 ;;
    esac
    if reason="$(oms_critical_path_reason "$path")"; then
      AFFECTED_MODE=full; AFFECTED_REASONS="$reason"; break
    fi
  done < "$changes"
  if [ "${OMS_AFFECTED_FULL:-0}" = 1 ]; then
    AFFECTED_MODE=full; AFFECTED_REASONS="forced:OMS_AFFECTED_FULL"
  elif ! git diff --quiet "$QUICK_TO" -- || [ -n "$(git ls-files --others --exclude-standard)" ]; then
    AFFECTED_MODE=full; AFFECTED_REASONS=workspace-differs-from-head; AFFECTED_NATIVE=true
  elif [ "$AFFECTED_MODE" = full ]; then
    :
  elif [ "$docs_only" = 1 ] && [ -s "$changes" ]; then
    AFFECTED_MODE=affected; AFFECTED_REASONS=documentation-only
  elif [ "$scoped_only" = 1 ] && [ -s "$changes" ]; then
    AFFECTED_MODE=affected; AFFECTED_REASONS=scoped-contracts
  else
    AFFECTED_MODE=full; AFFECTED_REASONS="changed-code-full"
  fi
  if [ "$AFFECTED_MODE" = affected ]; then
    RUN_QUICK=1
  elif [ "$CI_PARITY" = 1 ]; then
    [ "$SKIP_LINT" = 1 ] || RUN_LINT=1
    RUN_QUICK=1
    echo "affected: full selection ($AFFECTED_REASONS) runs lint and quick lane" >&2
  else
    [ "$SKIP_LINT" = 1 ] || RUN_LINT=1
    RUN_FOCUSED=1
    RUN_RUST=1
    echo "affected: full test fallback ($AFFECTED_REASONS)" >&2
  fi
}

[ "$CI_PARITY" != 1 ] || [ "$LIST_STAGES" = 1 ] || hide_provider_clis

if [ "$RUN_AFFECTED" = 1 ]; then
  prepare_affected_gate
  if [ -n "$CI_OUTPUT" ]; then
    printf 'mode=%s\nnative=%s\npython=false\noperator=\n' "$AFFECTED_MODE" "$AFFECTED_NATIVE" >> "$CI_OUTPUT"
    [ "$PRINT_AFFECTED_MODE" = 1 ] && { verify_oms_state; exit 0; }
    [ "$AFFECTED_MODE" != full ] || { verify_oms_state; exit 0; }
  elif [ "$PRINT_AFFECTED_MODE" = 1 ]; then
    echo "$AFFECTED_MODE"; verify_oms_state; exit 0
  fi
fi

if [ "$RUN_LINT" = 1 ]; then
  stage shellcheck lint_shell install.sh scripts/oms scripts/*.sh \
    scripts/lib/*.sh plugins/oh-my-setting/scripts/*.sh docs/examples/*.sh \
    templates/*.sh tests/*.sh tests/lib/*.sh
  stage skill-manifest bash scripts/install-skills.sh
  stage doc-references bash tests/source-distribution-smoke.sh --docs-only
  stage rust-migration-check cargo +1.99.1 run --manifest-path Cargo.toml -p oms-migrate --locked --offline -- check --repo .
fi

if [ "$RUN_FOCUSED" = 1 ]; then
  stage source-distribution bash tests/source-distribution-smoke.sh
  stage advisor-routing bash tests/advisor-routing-smoke.sh
  stage advisor-session bash tests/advisor-session-smoke.sh
  stage self-advice-disclosure bash tests/self-advice-disclosure-smoke.sh
  stage seat-reliability bash tests/seat-reliability-smoke.sh
  stage install-profile bash tests/install-profile-smoke.sh
  stage install-lifecycle-lock bash tests/install-lifecycle-lock-smoke.sh
  stage bsd-portability bash tests/bsd-portability-smoke.sh
  stage install-lifecycle bash tests/install-lifecycle-smoke.sh
fi

if [ "$RUN_RUST" = 1 ]; then
  stage rust-workspace-tests bash -c '
    cargo +1.99.1 test --manifest-path Cargo.toml --workspace --all-targets --locked --offline &&
      cargo +1.99.1 test --manifest-path Cargo.toml --workspace --doc --locked --offline
  '
fi

if [ "$RUN_INVARIANTS" = 1 ] || [ "$RUN_QUICK" = 1 ]; then
  stage invariants-skill-manifest bash scripts/install-skills.sh
  stage invariants-doc-references bash tests/source-distribution-smoke.sh --docs-only
  stage invariants-migration-check cargo +1.99.1 run --manifest-path Cargo.toml -p oms-migrate --locked --offline -- check --repo .
  stage invariants-source-distribution bash tests/source-distribution-smoke.sh
fi

if [ "$LIST_STAGES" != 1 ]; then
  verify_oms_state
fi
echo "check: ok"
