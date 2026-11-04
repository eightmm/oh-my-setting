#!/usr/bin/env bash
set -euo pipefail

# Full or partial pre-push feedback. Quick mode requires the complete GitHub
# Actions `gate` job; update records preserve the exact old/new commit range.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
gate_mode=quick
if [ "${1:-}" = --full ]; then
  gate_mode=full
  shift
fi
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
record_count=0
tag_only=1
malformed=0
local_ref=""
local_oid=""
remote_ref=""
remote_oid=""
remote_refs=""
if git -C "$ROOT" check-ref-format "refs/remotes/$REMOTE_NAME/probe" >/dev/null 2>&1; then
  remote_refs="$(git -C "$ROOT" for-each-ref --format='%(objectname)' "refs/remotes/$REMOTE_NAME/" 2>/dev/null)" || tag_only=0
else
  tag_only=0
fi
while IFS=' ' read -r next_local_ref next_local_oid next_remote_ref next_remote_oid extra ||
  [ -n "${next_local_ref:-}${next_local_oid:-}${next_remote_ref:-}${next_remote_oid:-}${extra:-}" ]; do
  record_count=$((record_count + 1))
  if [ -z "${next_local_ref:-}" ] || [ -z "${next_local_oid:-}" ] ||
    [ -z "${next_remote_ref:-}" ] || [ -z "${next_remote_oid:-}" ] || [ -n "${extra:-}" ]; then
    malformed=1
    tag_only=0
    continue
  fi
  case "$next_local_ref:$next_remote_ref" in
    refs/tags/*:refs/tags/*) ;;
    *) tag_only=0 ;;
  esac
  if is_zero_oid "$next_local_oid"; then
    tag_only=0
    continue
  fi
  if ! is_zero_oid "$next_remote_oid" && [ "$next_remote_oid" != "$next_local_oid" ]; then
    tag_only=0
  fi
  if [ "$tag_only" -eq 1 ]; then
    commit="$(git -C "$ROOT" rev-parse --verify "$next_local_oid^{commit}" 2>/dev/null)" || commit=""
    published=0
    if [ -n "$commit" ]; then
      for remote_tip in $remote_refs; do
        if git -C "$ROOT" merge-base --is-ancestor "$commit" "$remote_tip" 2>/dev/null; then
          published=1
          break
        fi
      done
    fi
    [ "$published" -eq 1 ] || tag_only=0
  fi
  update_count=$((update_count + 1))
  local_ref="$next_local_ref"
  local_oid="$next_local_oid"
  remote_ref="$next_remote_ref"
  remote_oid="$next_remote_oid"
done

if [ "$record_count" -gt 0 ] && [ "$tag_only" -eq 1 ]; then
  echo "pre-push: tag-only push of published commits; gate skipped"
  exit 0
fi
if [ "$gate_mode" = full ] || [ "$malformed" -eq 1 ]; then
  exec "$ROOT/scripts/check.sh"
fi

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
