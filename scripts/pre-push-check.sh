#!/usr/bin/env bash
set -euo pipefail

# Fast, partial pre-push feedback for repositories whose protected branch
# requires the complete GitHub Actions `gate` job. The hook supplies update
# records on stdin; this script preserves the exact old/new commit range.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REMOTE_NAME="${1:-origin}"

is_zero_oid() {
  case "$1" in
    ""|*[!0]*) return 1 ;;
    *) return 0 ;;
  esac
}

run_full_gate() {
  echo "pre-push: $1; running the full local gate" >&2
  exec "$ROOT/scripts/check.sh"
}

update_count=0
local_ref=""
local_oid=""
remote_ref=""
remote_oid=""
while IFS=' ' read -r next_local_ref next_local_oid next_remote_ref next_remote_oid; do
  [ -n "$next_local_ref" ] || continue
  if is_zero_oid "$next_local_oid"; then
    continue
  fi
  update_count=$((update_count + 1))
  local_ref="$next_local_ref"
  local_oid="$next_local_oid"
  remote_ref="$next_remote_ref"
  remote_oid="$next_remote_oid"
done

if [ "$update_count" -eq 0 ]; then
  echo "pre-push: deletion-only update; no local gate needed"
  exit 0
fi
if [ "$update_count" -ne 1 ]; then
  run_full_gate "multiple refs are being pushed"
fi
case "$local_ref" in
  refs/heads/*) ;;
  *) run_full_gate "a non-branch ref is being pushed" ;;
esac
case "$remote_ref" in
  refs/heads/*) ;;
  *) run_full_gate "the destination is not a branch ref" ;;
esac

git -C "$ROOT" cat-file -e "$local_oid^{commit}" 2>/dev/null ||
  run_full_gate "the pushed commit is unavailable locally"
head_oid="$(git -C "$ROOT" rev-parse HEAD)"
if [ "$local_oid" != "$head_oid" ]; then
  run_full_gate "the pushed tip is not the checked-out HEAD"
fi
if ! git -C "$ROOT" diff --quiet "$local_oid" --; then
  echo "pre-push: quick mode requires a clean tracked working tree" >&2
  echo "commit or stash tracked changes so the checked files equal the pushed commit" >&2
  exit 1
fi

if ! is_zero_oid "$remote_oid"; then
  git -C "$ROOT" cat-file -e "$remote_oid^{tree}" 2>/dev/null ||
    run_full_gate "the remote base is unavailable locally"
  base_oid="$remote_oid"
else
  base_oid=""
  remote_main="refs/remotes/$REMOTE_NAME/main"
  if git -C "$ROOT" show-ref --verify --quiet "$remote_main"; then
    base_oid="$(git -C "$ROOT" merge-base "$local_oid" "$remote_main" 2>/dev/null || true)"
  fi
  if [ -z "$base_oid" ]; then
    base_oid="$(git -C "$ROOT" hash-object -t tree /dev/null)"
  fi
fi

echo "pre-push: quick local checks only; the full GitHub Actions gate remains required"
exec "$ROOT/scripts/check.sh" --quick --changed-from "$base_oid" --changed-to "$local_oid"
