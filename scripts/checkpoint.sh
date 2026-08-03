#!/usr/bin/env bash
set -euo pipefail

# Reversible snapshots of tracked Git state. A checkpoint records the index
# (HEAD -> index) separately from unstaged changes (index -> worktree), so a
# restore preserves what was staged. Untracked files are deliberately left
# alone. Restore is a dry-run unless --apply is explicit and always creates a
# recovery checkpoint before touching tracked files.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"

REPO="$PWD"
ACTION=""
CHECKPOINT_ID=""
LABEL=""
AS_JSON=0
APPLY=0
CHECKPOINT_ROOT=""

usage() {
  cat <<'EOF'
Usage: checkpoint.sh [--repo PATH] <create|list|verify|restore> [ID] [options]

Create and restore local snapshots of tracked staged/unstaged Git state.
Checkpoints live under REPO/.oms/checkpoints and are never committed.
Untracked files are counted for visibility but are not captured or modified.

Commands:
  create              Save the current index and tracked worktree changes.
  list                List checkpoints, newest first.
  verify ID           Verify metadata, hashes, and patch applicability in a
                      temporary detached worktree.
  restore ID          Validate a restore without changing files (default).
  restore ID --apply  Create an automatic backup, then replace tracked index
                      and worktree state. Requires the same HEAD as creation.

Options:
  --repo PATH  Git repository. Default: current directory.
  --label TEXT Short human label for create.
  --json       Emit JSON (list emits one object per line).
  --apply      Required to perform restore; otherwise restore is a dry-run.
  -h, --help   Show this help.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

file_sha256() {
  python3 - "$1" <<'PY'
import hashlib, sys
with open(sys.argv[1], "rb") as handle:
    print(hashlib.sha256(handle.read()).hexdigest())
PY
}

write_index_patch() {
  local output="$1"
  git -C "$REPO" diff --cached --binary --full-index --no-ext-diff \
    --no-textconv HEAD -- > "$output"
}

write_worktree_patch() {
  local output="$1"
  git -C "$REPO" diff --binary --full-index --no-ext-diff --no-textconv -- \
    > "$output"
}

ensure_supported_tree() {
  local unmerged
  git -C "$REPO" rev-parse --verify HEAD >/dev/null 2>&1 || {
    echo "error: checkpoints require a repository with an initial commit" >&2
    return 2
  }
  unmerged="$(git -C "$REPO" diff --name-only --diff-filter=U -- 2>/dev/null || true)"
  [ -z "$unmerged" ] || {
    echo "error: resolve unmerged paths before creating or restoring a checkpoint" >&2
    return 2
  }
}

valid_id() {
  case "$1" in
    cp-*) ;;
    *) return 1 ;;
  esac
  case "$1" in
    *[!A-Za-z0-9._-]*|cp-) return 1 ;;
    *) return 0 ;;
  esac
}

checkpoint_dir() {
  valid_id "$1" || return 2
  printf '%s/%s\n' "$CHECKPOINT_ROOT" "$1"
}

checkpoint_metadata_head() {
  local dir="$1"
  local expected_id="$2"
  python3 - "$dir" "$expected_id" <<'PY'
import hashlib
import json
import os
import re
import sys

directory, expected_id = sys.argv[1:]
try:
    metadata_path = os.path.join(directory, "meta.json")
    if os.path.islink(metadata_path):
        raise ValueError("metadata must not be a symlink")
    with open(metadata_path, encoding="utf-8") as handle:
        metadata = json.load(handle)
    if metadata.get("schema") != 1 or metadata.get("id") != expected_id:
        raise ValueError("metadata identity mismatch")
    head = str(metadata.get("head") or "")
    if not re.fullmatch(r"[0-9a-f]{40,64}", head):
        raise ValueError("invalid saved HEAD")
    for name, key in (
        ("index.patch", "index_sha256"),
        ("worktree.patch", "worktree_sha256"),
    ):
        path = os.path.join(directory, name)
        if os.path.islink(path) or not os.path.isfile(path):
            raise ValueError("missing or unsafe %s" % name)
        with open(path, "rb") as handle:
            digest = hashlib.sha256(handle.read()).hexdigest()
        if digest != metadata.get(key):
            raise ValueError("checksum mismatch for %s" % name)
    print(head)
except (OSError, ValueError, json.JSONDecodeError) as error:
    print("error: invalid checkpoint: %s" % error, file=sys.stderr)
    raise SystemExit(1)
PY
}

checkpoint_create_raw() {
  local label="$1"
  local id head created tmp index_hash worktree_hash untracked_count

  ensure_supported_tree || return $?
  mkdir -p "$CHECKPOINT_ROOT"
  chmod 700 "$CHECKPOINT_ROOT" 2>/dev/null || true
  agent_memory_ensure_oms_ignore_for_path "$CHECKPOINT_ROOT/state" 2>/dev/null || true

  id="cp-$(date -u +%Y%m%dT%H%M%SZ)-$$-${RANDOM:-0}"
  while [ -e "$CHECKPOINT_ROOT/$id" ]; do
    id="cp-$(date -u +%Y%m%dT%H%M%SZ)-$$-${RANDOM:-0}"
  done
  tmp="$(mktemp -d "$CHECKPOINT_ROOT/.create.XXXXXX")" || return 1
  head="$(git -C "$REPO" rev-parse HEAD | tr -d '\r')"
  created="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

  if ! write_index_patch "$tmp/index.patch" ||
     ! write_worktree_patch "$tmp/worktree.patch"; then
    rm -rf "$tmp"
    return 1
  fi
  index_hash="$(file_sha256 "$tmp/index.patch" | tr -d '\r')"
  worktree_hash="$(file_sha256 "$tmp/worktree.patch" | tr -d '\r')"
  untracked_count="$(git -C "$REPO" ls-files --others --exclude-standard -- |
    awk 'END { print NR + 0 }' | tr -d '\r')"

  if ! python3 - "$tmp/meta.json" "$id" "$created" "$head" "$label" \
      "$index_hash" "$worktree_hash" "$untracked_count" <<'PY'
import json, os, sys

path, ident, created, head, label, index_hash, worktree_hash, untracked = sys.argv[1:]
payload = {
    "schema": 1,
    "id": ident,
    "created_at": created,
    "head": head,
    "label": label,
    "index_sha256": index_hash,
    "worktree_sha256": worktree_hash,
    "staged_bytes": os.path.getsize(os.path.join(os.path.dirname(path), "index.patch")),
    "unstaged_bytes": os.path.getsize(os.path.join(os.path.dirname(path), "worktree.patch")),
    "untracked_count": int(untracked),
}
with open(path, "w", encoding="utf-8", newline="\n") as handle:
    json.dump(payload, handle, ensure_ascii=False, sort_keys=True)
    handle.write("\n")
PY
  then
    rm -rf "$tmp"
    return 1
  fi
  chmod 600 "$tmp/meta.json" "$tmp/index.patch" "$tmp/worktree.patch" 2>/dev/null || true
  if ! mv "$tmp" "$CHECKPOINT_ROOT/$id"; then
    rm -rf "$tmp"
    return 1
  fi
  printf '%s\n' "$id"
}

checkpoint_verify_raw() {
  local id="$1"
  local dir head tmp worktree added=0 rc=0

  dir="$(checkpoint_dir "$id")" || return 2
  if [ ! -d "$dir" ] || [ -L "$dir" ]; then
    echo "error: checkpoint not found or unsafe: $id" >&2
    return 2
  fi
  head="$(checkpoint_metadata_head "$dir" "$id")" || return 1
  git -C "$REPO" cat-file -e "$head^{commit}" 2>/dev/null || {
    echo "error: checkpoint commit is not available locally: $head" >&2
    return 1
  }

  tmp="$(mktemp -d "${TMPDIR:-/tmp}/oms-checkpoint-verify.XXXXXX")" || return 1
  worktree="$tmp/worktree"
  if git -C "$REPO" worktree add --detach --quiet "$worktree" "$head" >/dev/null 2>&1; then
    added=1
  else
    echo "error: could not create a temporary worktree to verify $id" >&2
    rc=1
  fi
  if [ "$rc" -eq 0 ] && [ -s "$dir/index.patch" ]; then
    git -C "$worktree" apply --index "$dir/index.patch" >/dev/null 2>&1 || rc=1
  fi
  if [ "$rc" -eq 0 ] && [ -s "$dir/worktree.patch" ]; then
    git -C "$worktree" apply "$dir/worktree.patch" >/dev/null 2>&1 || rc=1
  fi
  if [ "$rc" -ne 0 ]; then
    echo "error: checkpoint patches do not apply cleanly to their saved HEAD: $id" >&2
  fi
  if [ "$added" -eq 1 ]; then
    git -C "$REPO" worktree remove --force "$worktree" >/dev/null 2>&1 || true
  fi
  rm -rf "$tmp"
  git -C "$REPO" worktree prune >/dev/null 2>&1 || true
  return "$rc"
}

tracked_state_matches() {
  local dir="$1"
  local tmp current_index current_worktree saved_index saved_worktree rc=0
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/oms-checkpoint-match.XXXXXX")" || return 1
  write_index_patch "$tmp/index.patch" || rc=1
  write_worktree_patch "$tmp/worktree.patch" || rc=1
  if [ "$rc" -eq 0 ]; then
    current_index="$(file_sha256 "$tmp/index.patch")"
    current_worktree="$(file_sha256 "$tmp/worktree.patch")"
    saved_index="$(file_sha256 "$dir/index.patch")"
    saved_worktree="$(file_sha256 "$dir/worktree.patch")"
    [ "$current_index" = "$saved_index" ] &&
      [ "$current_worktree" = "$saved_worktree" ] || rc=1
  fi
  rm -rf "$tmp"
  return "$rc"
}

replace_tracked_state() {
  local target_dir="$1"
  local tmp rc=0
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/oms-checkpoint-current.XXXXXX")" || return 1
  write_index_patch "$tmp/index.patch" || rc=1
  write_worktree_patch "$tmp/worktree.patch" || rc=1

  # Undo unstaged state first (worktree -> index), then staged state
  # (index/worktree -> HEAD). This avoids reset/checkout and leaves untracked
  # files untouched.
  if [ "$rc" -eq 0 ] && [ -s "$tmp/worktree.patch" ]; then
    git -C "$REPO" apply --reverse "$tmp/worktree.patch" || rc=1
  fi
  if [ "$rc" -eq 0 ] && [ -s "$tmp/index.patch" ]; then
    git -C "$REPO" apply --reverse --index "$tmp/index.patch" || rc=1
  fi
  if [ "$rc" -eq 0 ] && [ -s "$target_dir/index.patch" ]; then
    git -C "$REPO" apply --index "$target_dir/index.patch" || rc=1
  fi
  if [ "$rc" -eq 0 ] && [ -s "$target_dir/worktree.patch" ]; then
    git -C "$REPO" apply "$target_dir/worktree.patch" || rc=1
  fi
  rm -rf "$tmp"
  [ "$rc" -eq 0 ] && tracked_state_matches "$target_dir"
}

cmd_create_locked() {
  local id
  id="$(checkpoint_create_raw "$LABEL")" || return $?
  if [ "$AS_JSON" -eq 1 ]; then
    cat "$CHECKPOINT_ROOT/$id/meta.json"
  else
    echo "checkpoint: created $id"
    echo "checkpoint: untracked files were not captured"
  fi
}

cmd_restore_locked() {
  local id="$CHECKPOINT_ID"
  local dir saved_head current_head backup_id backup_dir
  dir="$(checkpoint_dir "$id")" || return 2
  if [ ! -d "$dir" ] || [ -L "$dir" ]; then
    echo "error: checkpoint not found or unsafe: $id" >&2
    return 2
  fi
  saved_head="$(checkpoint_metadata_head "$dir" "$id")" || return 1
  current_head="$(git -C "$REPO" rev-parse HEAD 2>/dev/null | tr -d '\r')"
  [ "$current_head" = "$saved_head" ] || {
    echo "error: checkpoint $id belongs to HEAD $saved_head; current HEAD is $current_head" >&2
    return 1
  }
  ensure_supported_tree || return $?
  checkpoint_verify_raw "$id" || return $?

  if [ "$APPLY" -eq 0 ]; then
    if [ "$AS_JSON" -eq 1 ]; then
      printf '{"schema":1,"id":"%s","dry_run":true,"applicable":true}\n' "$id"
    else
      echo "checkpoint: dry-run ok for $id (pass --apply to restore)"
      echo "checkpoint: untracked files will remain untouched"
    fi
    return 0
  fi

  backup_id="$(checkpoint_create_raw "auto-backup-before-$id")" || {
    echo "error: could not create the mandatory recovery checkpoint" >&2
    return 1
  }
  backup_dir="$CHECKPOINT_ROOT/$backup_id"
  if replace_tracked_state "$dir"; then
    if [ "$AS_JSON" -eq 1 ]; then
      printf '{"schema":1,"id":"%s","applied":true,"backup_id":"%s"}\n' \
        "$id" "$backup_id"
    else
      echo "checkpoint: restored $id"
      echo "checkpoint: recovery backup $backup_id"
    fi
    return 0
  fi

  echo "error: restore failed; attempting recovery from $backup_id" >&2
  if replace_tracked_state "$backup_dir"; then
    echo "checkpoint: original tracked state recovered from $backup_id" >&2
  else
    echo "error: automatic recovery also failed; checkpoint $backup_id remains available" >&2
  fi
  return 1
}

cmd_list() {
  local found=0 meta i
  local -a metas=()
  [ -d "$CHECKPOINT_ROOT" ] || return 0
  for meta in "$CHECKPOINT_ROOT"/*/meta.json; do
    [ -f "$meta" ] || continue
    metas+=("$meta")
  done
  i=$((${#metas[@]} - 1))
  while [ "$i" -ge 0 ]; do
    meta="${metas[$i]}"
    found=1
    if [ "$AS_JSON" -eq 1 ]; then
      cat "$meta"
    else
      python3 - "$meta" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    row = json.load(handle)
label = " — %s" % row["label"] if row.get("label") else ""
print("%s  %s%s" % (row["id"], row["created_at"], label))
PY
    fi
    i=$((i - 1))
  done
  if [ "$found" -eq 0 ] && [ "$AS_JSON" -eq 0 ]; then
    echo "checkpoint: none"
  fi
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --label) [ "$#" -ge 2 ] || fail "--label requires text"; LABEL="$2"; shift 2 ;;
    --json) AS_JSON=1; shift ;;
    --apply) APPLY=1; shift ;;
    create|list|verify|restore)
      [ -z "$ACTION" ] || fail "only one command may be used"
      ACTION="$1"
      shift
      ;;
    -h|--help) usage; exit 0 ;;
    --*) fail "unknown option: $1" ;;
    *)
      case "$ACTION" in
        verify|restore)
          [ -z "$CHECKPOINT_ID" ] || fail "unexpected argument: $1"
          CHECKPOINT_ID="$1"
          shift
          ;;
        *) fail "unexpected argument: $1" ;;
      esac
      ;;
  esac
done

[ -n "$ACTION" ] || { usage >&2; exit 2; }
command -v python3 >/dev/null 2>&1 || fail "python3 is required"
REPO="$(oms_repo_root "$REPO")" || fail "bad --repo"
git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || fail "not a Git repository: $REPO"
CHECKPOINT_ROOT="$REPO/.oms/checkpoints"
[ ! -L "$CHECKPOINT_ROOT" ] || fail "unsafe symlink at $CHECKPOINT_ROOT"

case "$LABEL" in
  *$'\n'*|*$'\r'*) fail "--label cannot contain newlines" ;;
esac
[ "$(printf '%s' "$LABEL" | wc -c | tr -d ' ')" -le 120 ] || fail "--label is limited to 120 bytes"
case "$ACTION" in
  create)
    [ -z "$CHECKPOINT_ID" ] || fail "create takes no checkpoint ID"
    [ "$APPLY" -eq 0 ] || fail "--apply applies only to restore"
    oms_with_file_lock "$CHECKPOINT_ROOT/.operation" cmd_create_locked
    ;;
  list)
    [ -z "$LABEL" ] || fail "--label applies only to create"
    [ "$APPLY" -eq 0 ] || fail "--apply applies only to restore"
    cmd_list
    ;;
  verify)
    [ -n "$CHECKPOINT_ID" ] || fail "verify requires an ID"
    [ -z "$LABEL" ] || fail "--label applies only to create"
    [ "$APPLY" -eq 0 ] || fail "--apply applies only to restore"
    checkpoint_verify_raw "$CHECKPOINT_ID"
    if [ "$AS_JSON" -eq 1 ]; then
      printf '{"schema":1,"id":"%s","verified":true}\n' "$CHECKPOINT_ID"
    else
      echo "checkpoint: verified $CHECKPOINT_ID"
    fi
    ;;
  restore)
    [ -n "$CHECKPOINT_ID" ] || fail "restore requires an ID"
    [ -z "$LABEL" ] || fail "--label applies only to create"
    oms_with_file_lock "$CHECKPOINT_ROOT/.operation" cmd_restore_locked
    ;;
esac
