#!/usr/bin/env bash
set -euo pipefail

# Fetch and fast-forward the shared checkout only between canonical snapshots.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"
# shellcheck source=scripts/lib/canonical-sync.sh
. "$ROOT/scripts/lib/canonical-sync.sh"

usage() {
  cat <<'EOF'
Usage: oms sync-shared [--repo PATH] [--remote NAME] [--target BRANCH]

Refresh the canonical shared checkout (defaults: current repo, origin, main).
Live calls defer sync with exit 75; retry after they finish. New calls wait up
to five seconds for sync, then refuse safely if it is still busy. Dirty,
detached and diverged checkouts are refused without a merge.
This operation does not push, gate, install or create a landing receipt.
EOF
}
fail() { echo "error: $*" >&2; exit 2; }
REPO="$PWD" REMOTE=origin TARGET=main
while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo|--remote|--target)
      [ "$#" -ge 2 ] || fail "$1 requires a value"
      case "$1" in --repo) REPO="$2" ;; --remote) REMOTE="$2" ;; --target) TARGET="$2" ;; esac
      shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done
case "$REMOTE" in ""|-*|*[!A-Za-z0-9._-]*) fail 'invalid remote name' ;; esac
git check-ref-format "refs/heads/$TARGET" >/dev/null || fail 'invalid target branch'
REPO="$(oms_state_root "$REPO")" || fail 'cannot resolve shared checkout'
REPO="${REPO//$'\r'/}"
REPO="$(cd "$REPO" && pwd -P)" || fail 'cannot resolve physical shared checkout'

sync_clean() {
  local status
  status="$(git -C "$REPO" status --porcelain --untracked-files=all)" || return 2
  [ -z "$status" ] || { echo 'error: dirty shared checkout; sync refused' >&2; return 2; }
}

sync_checkout() {
  local ref="refs/remotes/$REMOTE/$TARGET"
  oms_git_assert_safe_execution_config "$REPO" || return 2
  oms_git_assert_plain_index "$REPO" || return 2
  # Any local branch may track the target; only a fast-forward to the fetched target is applied.
  git -C "$REPO" symbolic-ref --quiet HEAD >/dev/null || {
    echo 'error: detached shared checkout; sync refused' >&2; return 2;
  }
  sync_clean || return $?
  git -C "$REPO" fetch --no-tags -- "$REMOTE" "+refs/heads/$TARGET:$ref" || return $?
  git -C "$REPO" merge-base --is-ancestor HEAD "$ref" || {
    echo 'error: shared checkout is ahead or diverged; sync refused' >&2; return 2;
  }
  sync_clean || return $?
  git -C "$REPO" merge --ff-only "$ref"
}

oms_canonical_sync_prepare "$REPO"
[ ! -L "$REPO/.oms/.gitignore" ] || fail 'symbolic .oms/.gitignore; sync refused'
agent_memory_ensure_oms_ignore_for_path "$REPO/.oms/locks/canonical-sync" || exit 2
oms_canonical_sync_gate "$REPO" oms_canonical_sync_if_idle "$REPO" sync_checkout
