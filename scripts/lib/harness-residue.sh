# shellcheck shell=bash
# Crash-residue helpers for oh-my-setting harness state. Sourced, not executed.

# shellcheck source=file-lock.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/file-lock.sh"

OMS_HARNESS_RESIDUE_REMOVED=0
OMS_HARNESS_RESIDUE_WOULD_REMOVE=0

oms_harness_residue_reset() {
  OMS_HARNESS_RESIDUE_REMOVED=0
  OMS_HARNESS_RESIDUE_WOULD_REMOVE=0
}

oms_harness_tmp_base() {
  printf '%s\n' "${TMPDIR:-/tmp}"
}

oms_harness_mark_tmpdir() {
  local dir="$1"
  local repo="$2"
  local worktree="$3"

  [ -d "$dir" ] || return 0
  {
    printf 'kind=oh-my-setting-temp\n'
    printf 'pid=%s\n' "$$"
    printf 'repo=%s\n' "$repo"
    printf 'worktree=%s\n' "$worktree"
    printf 'temporary=1\n'
  } > "$dir/.oh-my-setting-tmp"
}

oms_harness_read_marker_value() {
  local file="$1"
  local key="$2"

  sed -n "s/^$key=//p" "$file" 2>/dev/null | sed -n '1p'
}

oms_harness_count_stale_worktrees() {
  local repo="$1"
  local count=0
  local path

  git -C "$repo" rev-parse --git-dir >/dev/null 2>&1 || {
    printf '0\n'
    return 0
  }
  while IFS= read -r path; do
    [ -n "$path" ] || continue
    [ -e "$path" ] || count=$((count + 1))
  done <<EOF
$(git -C "$repo" worktree list --porcelain 2>/dev/null | sed -n 's/^worktree //p')
EOF
  printf '%s\n' "$count"
}

oms_harness_prune_stale_worktrees() {
  local repo="$1"
  local dry_run="${2:-0}"
  local before

  before="$(oms_harness_count_stale_worktrees "$repo")"
  [ "$before" -gt 0 ] || return 0
  if [ "$dry_run" = "1" ]; then
    printf 'would prune: %s stale git worktree registration(s)\n' "$before"
    OMS_HARNESS_RESIDUE_WOULD_REMOVE=$((OMS_HARNESS_RESIDUE_WOULD_REMOVE + before))
  else
    git -C "$repo" worktree prune >/dev/null 2>&1 || true
    printf 'pruned: %s stale git worktree registration(s)\n' "$before"
    OMS_HARNESS_RESIDUE_REMOVED=$((OMS_HARNESS_RESIDUE_REMOVED + before))
  fi
}

oms_harness_lock_residue_count() {
  local lock_root
  local lock_dir
  local pid
  local count=0

  lock_root="$(oms_file_lock_dir)"
  [ -d "$lock_root" ] || {
    printf '0\n'
    return 0
  }
  for lock_dir in "$lock_root"/*.lock; do
    [ -d "$lock_dir" ] || continue
    [ -f "$lock_dir/pid" ] || continue
    pid="$(sed -n '1p' "$lock_dir/pid" 2>/dev/null || true)"
    if ! oms_file_lock_pid_alive "$pid"; then
      count=$((count + 1))
    fi
  done
  printf '%s\n' "$count"
}

# flock-path locks are regular files, and the design never unlinks them: a
# released flock file cannot be removed safely, because a waiter blocked on the
# old inode and a new arrival creating a fresh one would both hold the lock.
# They are therefore permanent by construction. Counting them anyway, because
# the residue scan above only sees mkdir-path directories — on this machine that
# meant reporting 0 while 12k files sat there, which reads as "nothing to clean"
# rather than "this kind is not cleanable". Growth is bounded by the number of
# distinct state-file paths ever locked; a run that leaks its lock dir into a
# real HOME (a test without OMS_LOCK_DIR) is what makes that number large.
oms_harness_lock_file_count() {
  local lock_root
  local lock_file
  local count=0

  lock_root="$(oms_file_lock_dir)"
  [ -d "$lock_root" ] || {
    printf '0\n'
    return 0
  }
  for lock_file in "$lock_root"/*.lock; do
    [ -f "$lock_file" ] || continue
    count=$((count + 1))
  done
  printf '%s\n' "$count"
}

oms_harness_cleanup_dead_locks() {
  local dry_run="${1:-0}"
  local lock_root
  local lock_dir
  local pid

  lock_root="$(oms_file_lock_dir)"
  [ -d "$lock_root" ] || return 0
  for lock_dir in "$lock_root"/*.lock; do
    [ -d "$lock_dir" ] || continue
    [ -f "$lock_dir/pid" ] || continue
    pid="$(sed -n '1p' "$lock_dir/pid" 2>/dev/null || true)"
    oms_file_lock_pid_alive "$pid" && continue
    if [ "$dry_run" = "1" ]; then
      printf 'would remove: %s (dead harness lock)\n' "$lock_dir"
      OMS_HARNESS_RESIDUE_WOULD_REMOVE=$((OMS_HARNESS_RESIDUE_WOULD_REMOVE + 1))
    else
      rm -rf "$lock_dir"
      printf 'removed: %s (dead harness lock)\n' "$lock_dir"
      OMS_HARNESS_RESIDUE_REMOVED=$((OMS_HARNESS_RESIDUE_REMOVED + 1))
    fi
  done
}

oms_harness_tmp_residue_count() {
  local base
  local dir
  local marker
  local pid
  local temporary
  local count=0

  base="$(oms_harness_tmp_base)"
  [ -d "$base" ] || {
    printf '0\n'
    return 0
  }
  for dir in "$base"/oh-my-setting-*; do
    [ -d "$dir" ] || continue
    # Markers live in shared TMPDIR; only trust dirs this user owns, or a
    # planted marker could point worktree removal at a real worktree.
    [ -O "$dir" ] || continue
    marker="$dir/.oh-my-setting-tmp"
    [ -f "$marker" ] || continue
    [ "$(oms_harness_read_marker_value "$marker" kind)" = "oh-my-setting-temp" ] || continue
    temporary="$(oms_harness_read_marker_value "$marker" temporary)"
    [ "$temporary" = "1" ] || continue
    pid="$(oms_harness_read_marker_value "$marker" pid)"
    if ! oms_file_lock_pid_alive "$pid"; then
      count=$((count + 1))
    fi
  done
  printf '%s\n' "$count"
}

oms_harness_cleanup_temp_dirs() {
  local dry_run="${1:-0}"
  local base
  local dir
  local marker
  local pid
  local repo
  local temporary
  local worktree

  base="$(oms_harness_tmp_base)"
  [ -d "$base" ] || return 0
  for dir in "$base"/oh-my-setting-*; do
    [ -d "$dir" ] || continue
    # Only act on dirs this user owns (see count function): a planted marker
    # in shared TMPDIR must not steer worktree removal at a real worktree.
    [ -O "$dir" ] || continue
    marker="$dir/.oh-my-setting-tmp"
    [ -f "$marker" ] || continue
    [ "$(oms_harness_read_marker_value "$marker" kind)" = "oh-my-setting-temp" ] || continue
    temporary="$(oms_harness_read_marker_value "$marker" temporary)"
    [ "$temporary" = "1" ] || continue
    pid="$(oms_harness_read_marker_value "$marker" pid)"
    oms_file_lock_pid_alive "$pid" && continue

    if [ "$dry_run" = "1" ]; then
      printf 'would remove: %s (dead harness temp dir)\n' "$dir"
      OMS_HARNESS_RESIDUE_WOULD_REMOVE=$((OMS_HARNESS_RESIDUE_WOULD_REMOVE + 1))
      continue
    fi

    repo="$(oms_harness_read_marker_value "$marker" repo)"
    worktree="$(oms_harness_read_marker_value "$marker" worktree)"
    if [ -n "$repo" ] && [ -n "$worktree" ] && git -C "$repo" rev-parse --git-dir >/dev/null 2>&1; then
      git -C "$repo" worktree remove --force "$worktree" >/dev/null 2>&1 || true
      git -C "$repo" worktree prune >/dev/null 2>&1 || true
    fi
    rm -rf "$dir"
    printf 'removed: %s (dead harness temp dir)\n' "$dir"
    OMS_HARNESS_RESIDUE_REMOVED=$((OMS_HARNESS_RESIDUE_REMOVED + 1))
  done
}

oms_harness_cleanup_residue() {
  local repo="${1:-}"
  local dry_run="${2:-0}"

  oms_harness_residue_reset
  if [ -n "$repo" ] && git -C "$repo" rev-parse --git-dir >/dev/null 2>&1; then
    oms_harness_prune_stale_worktrees "$repo" "$dry_run"
  fi
  oms_harness_cleanup_dead_locks "$dry_run"
  oms_harness_cleanup_temp_dirs "$dry_run"
}

oms_harness_count_unindexed_artifacts() {
  local repo="$1"
  local artifacts_dir="$repo/.oms/artifacts"
  local index="$artifacts_dir/index.jsonl"

  [ -d "$artifacts_dir" ] || {
    printf '0\n'
    return 0
  }
  command -v python3 >/dev/null 2>&1 || {
    printf '0\n'
    return 0
  }
  # Count with the same grace window prune --files deletes with. A live
  # provider call writes its artifact before the index row lands, so counting
  # age-blind makes doctor warn about in-flight work and then prescribe a
  # command that correctly deletes nothing — a remedy that cannot act on the
  # complaint.
  python3 - "$repo" "$artifacts_dir" "$index" \
    "${OMS_ARTIFACT_ORPHAN_GRACE:-86400}" <<'PY'
import json
import os
import sys
import time

repo, artifacts_dir, index, grace_raw = sys.argv[1:]
try:
    grace = max(0, int(grace_raw))
except ValueError:
    grace = 86400
now = time.time()
tracked = set()
if os.path.exists(index):
    with open(index, "r", encoding="utf-8") as f:
        for line in f:
            try:
                row = json.loads(line)
            except Exception:
                continue
            if not isinstance(row, dict):
                continue
            for key in ("artifact", "patch", "source"):
                value = row.get(key)
                if not isinstance(value, str) or not value:
                    continue
                path = value if os.path.isabs(value) else os.path.join(repo, value)
                tracked.add(os.path.realpath(path))

count = 0
for root, dirs, files in os.walk(artifacts_dir):
    dirs[:] = [d for d in dirs if not d.startswith(".")]
    for name in files:
        if not (name.endswith(".md") or name.endswith(".patch")):
            continue
        path = os.path.join(root, name)
        if os.path.realpath(path) in tracked:
            continue
        try:
            if now - os.stat(path).st_mtime < grace:
                continue
        except OSError:
            continue
        count += 1
print(count)
PY
}
