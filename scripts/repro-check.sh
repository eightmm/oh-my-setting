#!/usr/bin/env bash
set -euo pipefail

# Prove a regression test reproduces: it must fail at a base commit and pass
# at HEAD, each run in its own detached worktree so the dirty tree never
# contaminates the verdict. A test that passes on both sides proves nothing
# about the fix — that is the exact anti-pattern this verb exists to refuse
# mechanically instead of by reviewer discipline. Exits 0 on a proven pair,
# 3 when the test already passes on the base, 4 when it fails on HEAD, 2 on
# setup errors. The verdict is appended to the artifact index (kind
# repro-check) with the bounded output of both runs.

# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib/peer-common.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/peer-common.sh"

REPO="$PWD"
TEST_CMD=""
BASE_REF="HEAD~1"

usage() {
  cat <<'EOF'
Usage: repro-check.sh [--repo PATH] --test CMD [--base REF]

Run CMD in a detached worktree at HEAD (must pass) and at REF (must fail).

  --repo PATH   Git repository (default: current directory).
  --test CMD    The regression test command, run with bash -c from the
                worktree root. Required.
  --base REF    The commit the test must fail on (default: HEAD~1 — the
                ordinary fix-plus-test-in-one-commit shape). For receipt-bound
                work pass the frozen base SHA.

Exit codes: 0 proven (fails on base, passes on HEAD); 3 the test passes on
the base too, so it proves nothing about the fix; 4 the test fails on HEAD;
2 setup or usage error. OMS_REPRO_TIMEOUT bounds each side (seconds,
default 600) when the timeout binary exists.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || fail "--repo requires path"; REPO="$2"; shift 2 ;;
    --test) [ "$#" -ge 2 ] || fail "--test requires a command"; TEST_CMD="$2"; shift 2 ;;
    --base) [ "$#" -ge 2 ] || fail "--base requires a git ref"; BASE_REF="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ -n "$TEST_CMD" ] || { usage >&2; fail "--test is required"; }
REPO="$(cd "$REPO" && pwd)" || fail "cannot resolve --repo"
git -C "$REPO" rev-parse --verify HEAD >/dev/null 2>&1 || fail "repo has no HEAD"

head_sha="$(git -C "$REPO" rev-parse HEAD)"
base_sha="$(git -C "$REPO" rev-parse --verify "$BASE_REF^{commit}" 2>/dev/null)" ||
  fail "base ref does not resolve to a commit: $BASE_REF"
[ "$head_sha" != "$base_sha" ] || fail "base and HEAD are the same commit; nothing to prove"

SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/oh-my-setting-repro.XXXXXX")" || fail "mktemp failed"
cleanup() {
  local side
  for side in head base; do
    [ -d "$SCRATCH/$side" ] &&
      git -C "$REPO" worktree remove --force "$SCRATCH/$side" >/dev/null 2>&1 || true
  done
  rm -rf "$SCRATCH"
}
trap cleanup EXIT HUP INT TERM

run_side() {
  # Runs the test at one commit; prints the exit code, keeps bounded output.
  local side="$1"
  local sha="$2"
  local rc=0
  GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_NOSYSTEM=1 \
    git -c core.hooksPath=/dev/null -c core.fsmonitor=false -C "$REPO" \
    worktree add --detach "$SCRATCH/$side" "$sha" >/dev/null 2>&1 ||
    fail "cannot create the $side worktree at ${sha:0:12}"
  if command -v timeout >/dev/null 2>&1; then
    ( cd "$SCRATCH/$side" &&
      timeout "${OMS_REPRO_TIMEOUT:-600}" bash -c "$TEST_CMD" ) \
      >"$SCRATCH/$side.out" 2>&1 || rc=$?
  else
    ( cd "$SCRATCH/$side" && bash -c "$TEST_CMD" ) \
      >"$SCRATCH/$side.out" 2>&1 || rc=$?
  fi
  printf '%s\n' "$rc"
}

head_exit="$(run_side head "$head_sha")"
base_exit="$(run_side base "$base_sha")"

verdict_exit=0
verdict="proven"
if [ "$head_exit" -ne 0 ]; then
  verdict_exit=4
  verdict="test-fails-on-head"
elif [ "$base_exit" -eq 0 ]; then
  verdict_exit=3
  verdict="test-passes-on-base"
fi

artifact_dir="$REPO/.oms/artifacts/repro-check"
mkdir -p "$artifact_dir"
artifact="$artifact_dir/repro-$(date -u +%Y%m%dT%H%M%SZ)-$$.md"
{
  printf '# repro-check\n\n'
  printf -- '- test: %s\n' "$TEST_CMD"
  printf -- '- head: %s exit=%s\n' "$head_sha" "$head_exit"
  printf -- '- base: %s (%s) exit=%s\n' "$base_sha" "$BASE_REF" "$base_exit"
  printf -- '- verdict: %s\n\n' "$verdict"
  printf '## head output (tail)\n\n```\n'
  tail -n 40 "$SCRATCH/head.out" 2>/dev/null || true
  printf '```\n\n## base output (tail)\n\n```\n'
  tail -n 40 "$SCRATCH/base.out" 2>/dev/null || true
  printf '```\n'
} > "$artifact"
ma_append_artifact_index "$REPO" repro-check local "$verdict_exit" "$artifact" || true

case "$verdict" in
  proven)
    echo "repro-check: pass head=${head_sha:0:12} base=${base_sha:0:12} base_exit=$base_exit"
    ;;
  test-fails-on-head)
    echo "repro-check: fail code=test-fails-on-head head_exit=$head_exit (see $artifact)" >&2
    echo "remedy: the fix itself is not green at HEAD; land the fix before proving the test" >&2
    ;;
  test-passes-on-base)
    echo "repro-check: fail code=test-passes-on-base base=${base_sha:0:12} (see $artifact)" >&2
    echo "remedy: a test that passes on the base proves nothing about the fix; tighten it until it goes red at $BASE_REF" >&2
    ;;
esac
exit "$verdict_exit"
