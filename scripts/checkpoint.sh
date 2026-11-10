#!/usr/bin/env bash
set -euo pipefail

# Reversible snapshots of tracked Git state. A checkpoint records the index
# (HEAD -> index) separately from unstaged changes (index -> worktree), so a
# restore preserves what was staged. Restore is a dry-run unless --apply is
# explicit and always creates a recovery checkpoint before touching tracked
# files.
#
# Untracked files are left alone unless `create --untracked` asks: then the
# regular untracked, unignored files that fit a size cap are copied into the
# checkpoint, and a restore writes back only the ones that are missing. A file
# that exists is never replaced, so restoring cannot lose newer work.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"

REPO="$PWD"
ACTION=""
CHECKPOINT_ID=""
LABEL=""
AS_JSON=0
APPLY=0
UNTRACKED=0
REUSE=0
UNTRACKED_ONLY=0
CHECKPOINT_ROOT=""
UNTRACKED_LIB="$ROOT/scripts/lib/checkpoint_untracked.py"

usage() {
  cat <<'EOF'
Usage: checkpoint.sh [--repo PATH] <create|list|verify|restore> [ID] [options]

Create and restore local snapshots of tracked staged/unstaged Git state.
Checkpoints live under REPO/.oms/checkpoints and are never committed.
Untracked files are counted for visibility; they are captured only with
create --untracked, and a restore never replaces a file that exists.

Commands:
  create              Save the current index and tracked worktree changes.
  list                List checkpoints, newest first.
  verify ID           Verify metadata, hashes, and reconstructed tracked state
                      in a temporary detached worktree.
  restore ID          Validate a restore without changing files (default).
  restore ID --apply  Create an automatic backup, then replace tracked index
                      and worktree state. Requires the same HEAD as creation.
                      Captured untracked files that are missing are written
                      back; existing files are left alone.

Options:
  --repo PATH  Git repository. Default: current directory.
  --label TEXT Short human label for create.
  --untracked  With create: also copy the regular untracked, unignored files
               (8 MiB a file, 64 MiB and 5000 files in all; the rest are
               counted as omitted). The copy stays in .oms/checkpoints.
  --reuse      With create: when the newest checkpoint with the same label
               already holds exactly this state, print its ID instead of
               creating another.
  --untracked-only
               With restore: write back missing untracked files only; tracked
               state and HEAD are not compared or changed.
  --json       Emit JSON (list emits one object per line).
  --apply      Required to perform restore; otherwise restore is a dry-run.
  -h, --help   Show this help.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

write_index_patch() {
  local output="$1"
  local repo="${2:-$REPO}"
  git -C "$repo" -c diff.noprefix=false -c color.ui=false diff \
    --no-color --src-prefix=a/ --dst-prefix=b/ --cached --binary \
    --full-index --no-ext-diff --no-textconv HEAD -- > "$output"
}

write_worktree_patch() {
  local output="$1"
  local repo="${2:-$REPO}"
  git -C "$repo" -c diff.noprefix=false -c color.ui=false diff \
    --no-color --src-prefix=a/ --dst-prefix=b/ --binary --full-index \
    --no-ext-diff --no-textconv -- > "$output"
}

ensure_no_intent_to_add() {
  python3 - "$REPO" <<'PY'
import re
import subprocess
import sys

result = subprocess.run(
    ["git", "-C", sys.argv[1], "ls-files", "--debug", "-z"],
    stdout=subprocess.PIPE, stderr=subprocess.PIPE,
)
if result.returncode:
    print("error: could not inspect the Git index", file=sys.stderr)
    raise SystemExit(2)

# -z places each pathname before a NUL, followed by its index metadata. The
# intent-to-add bit is absent from --stage and --status, including for empty
# files; --debug exposes it without changing the index.
for entry in result.stdout.split(b"\0")[1:]:
    match = re.match(
        rb"  ctime:[^\n]*\n  mtime:[^\n]*\n  dev:[^\n]*\n"
        rb"  uid:[^\n]*\n  size:[^\n]*\tflags: ([0-9a-fA-F]+)\n",
        entry,
    )
    if match is None:
        print("error: could not read Git index flags", file=sys.stderr)
        raise SystemExit(2)
    if int(match.group(1), 16) & 0x20000000:
        print("error: intent-to-add entries cannot be checkpointed or restored", file=sys.stderr)
        raise SystemExit(2)
PY
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
  ensure_no_intent_to_add
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
    archive = os.path.join(directory, "untracked.tar")
    if metadata.get("untracked_sha256") or os.path.lexists(archive):
        if os.path.islink(archive) or not os.path.isfile(archive):
            raise ValueError("missing or unsafe untracked.tar")
        with open(archive, "rb") as handle:
            if hashlib.sha256(handle.read()).hexdigest() != metadata.get("untracked_sha256"):
                raise ValueError("checksum mismatch for untracked.tar")
    print(head)
except (OSError, ValueError, json.JSONDecodeError) as error:
    print("error: invalid checkpoint: %s" % error, file=sys.stderr)
    raise SystemExit(1)
PY
}

# Prints the new checkpoint's ID, or `reused:ID` when REUSE (third argument)
# found the newest checkpoint with this label already holding exactly this state.
checkpoint_create_raw() {
  local label="$1"
  local with_untracked="${2:-0}"
  local reuse="${3:-0}"
  local id head created tmp index_hash worktree_hash untracked_count
  local capture="" fingerprint="-" existing=""

  ensure_supported_tree || return $?
  mkdir -p "$CHECKPOINT_ROOT"
  chmod 700 "$CHECKPOINT_ROOT" 2>/dev/null || true
  agent_memory_ensure_oms_ignore_for_path "$CHECKPOINT_ROOT/state" 2>/dev/null || true

  id="cp-$(date -u +%Y%m%dT%H%M%SZ)-$$-${RANDOM:-0}"
  while [ -e "$CHECKPOINT_ROOT/$id" ]; do
    id="cp-$(date -u +%Y%m%dT%H%M%SZ)-$$-${RANDOM:-0}"
  done
  tmp="$(mktemp -d "$CHECKPOINT_ROOT/.create.XXXXXX")" || return 1
  head="$(git -C "$REPO" rev-parse HEAD)"
  created="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

  if ! write_index_patch "$tmp/index.patch" ||
     ! write_worktree_patch "$tmp/worktree.patch"; then
    rm -rf "$tmp"
    return 1
  fi
  index_hash="$(oms_sha256_file "$tmp/index.patch")"
  worktree_hash="$(oms_sha256_file "$tmp/worktree.patch")"
  untracked_count="$(git -C "$REPO" ls-files --others --exclude-standard -- |
    awk 'END { print NR + 0 }')"
  if [ "$with_untracked" -eq 1 ]; then
    if ! capture="$(python3 "$UNTRACKED_LIB" capture "$REPO" "$tmp")" || [ -z "$capture" ]; then
      echo "error: could not capture untracked files" >&2
      rm -rf "$tmp"
      return 1
    fi
    fingerprint="$(OMS_CAPTURE="$capture" python3 -c \
      'import json, os; print(json.loads(os.environ["OMS_CAPTURE"])["untracked_fingerprint"])')" || {
      rm -rf "$tmp"
      return 1
    }
  fi
  if [ "$reuse" -eq 1 ]; then
    existing="$(python3 "$UNTRACKED_LIB" match "$CHECKPOINT_ROOT" "$label" "$head" \
      "$index_hash" "$worktree_hash" "$fingerprint" 2>/dev/null |
      python3 -c 'import json, sys; print(json.load(sys.stdin).get("id") or "")' 2>/dev/null || true)"
    if [ -n "$existing" ] && valid_id "$existing"; then
      rm -rf "$tmp"
      printf 'reused:%s\n' "$existing"
      return 0
    fi
  fi

  if ! OMS_CAPTURE="$capture" python3 - "$tmp/meta.json" "$id" "$created" "$head" "$label" \
      "$index_hash" "$worktree_hash" "$untracked_count" <<'PY'
import json, os, sys

path, ident, created, head, label, index_hash, worktree_hash, untracked = sys.argv[1:]
capture = json.loads(os.environ.get("OMS_CAPTURE") or "{}")
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
payload.update(capture)
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
  if oms_with_worktree_registry_lock "$REPO" git -C "$REPO" worktree add --detach --quiet "$worktree" "$head" >/dev/null 2>&1; then
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
  if [ "$rc" -eq 0 ]; then
    write_index_patch "$tmp/index.patch" "$worktree" || rc=1
    write_worktree_patch "$tmp/worktree.patch" "$worktree" || rc=1
    if [ "$rc" -eq 0 ]; then
      cmp -s "$dir/index.patch" "$tmp/index.patch" &&
        cmp -s "$dir/worktree.patch" "$tmp/worktree.patch" || rc=1
    fi
  fi
  if [ "$rc" -ne 0 ]; then
    echo "error: checkpoint patches cannot reproduce their saved tracked state: $id" >&2
  fi
  if [ "$added" -eq 1 ]; then
    oms_with_worktree_registry_lock "$REPO" git -C "$REPO" worktree remove --force "$worktree" >/dev/null 2>&1 || true
  fi
  rm -rf "$tmp"
  oms_with_worktree_registry_lock "$REPO" git -C "$REPO" worktree prune >/dev/null 2>&1 || true
  return "$rc"
}

tracked_state_matches() {
  local dir="$1"
  local tmp current_index current_worktree saved_index saved_worktree rc=0
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/oms-checkpoint-match.XXXXXX")" || return 1
  write_index_patch "$tmp/index.patch" || rc=1
  write_worktree_patch "$tmp/worktree.patch" || rc=1
  if [ "$rc" -eq 0 ]; then
    current_index="$(oms_sha256_file "$tmp/index.patch")"
    current_worktree="$(oms_sha256_file "$tmp/worktree.patch")"
    saved_index="$(oms_sha256_file "$dir/index.patch")"
    saved_worktree="$(oms_sha256_file "$dir/worktree.patch")"
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
  # Reversing the worktree patch can make its bytes match the index while
  # leaving cached stat data stale. Refresh before applying the index patch.
  if [ "$rc" -eq 0 ] && {
    [ -s "$tmp/index.patch" ] || [ -s "$target_dir/index.patch" ]
  }; then
    git -C "$REPO" update-index --refresh || rc=1
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

# One line on what a checkpoint's untracked capture holds, or holds back.
untracked_capture_line() {
  python3 - "$1/meta.json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    meta = json.load(handle)
if "untracked_captured" not in meta:
    print("checkpoint: untracked files were not captured")
else:
    omitted = meta.get("untracked_omitted", 0)
    print("checkpoint: captured %d untracked file(s)%s" % (
        meta["untracked_captured"],
        "; %d omitted (over the size cap or not a regular file)" % omitted if omitted else ""))
PY
}

cmd_create_locked() {
  local id verb=created
  id="$(checkpoint_create_raw "$LABEL" "$UNTRACKED" "$REUSE")" || return $?
  case "$id" in
    reused:*) id="${id#reused:}"; verb=reused ;;
  esac
  if [ "$AS_JSON" -eq 1 ]; then
    cat "$CHECKPOINT_ROOT/$id/meta.json"
  else
    echo "checkpoint: $verb $id"
    untracked_capture_line "$CHECKPOINT_ROOT/$id"
  fi
}

# Write back the captured untracked files that are missing (dry unless APPLY).
# Prints the report as JSON; an existing file is never replaced.
untracked_restore_report() {
  local dir="$1"
  if [ "$APPLY" -eq 1 ]; then
    python3 "$UNTRACKED_LIB" restore "$REPO" "$dir/untracked.tar" --apply
  else
    python3 "$UNTRACKED_LIB" restore "$REPO" "$dir/untracked.tar"
  fi
}

untracked_restore_line() {
  OMS_REPORT="$1" python3 -c '
import json, os
r = json.loads(os.environ["OMS_REPORT"])
if r["applied"]:
    print("checkpoint: wrote back %d missing untracked file(s); %d already present were left alone%s" % (
        r["written"], r["present"], "; %d refused" % r["refused"] if r["refused"] else ""))
else:
    print("checkpoint: %d captured untracked file(s) are missing and would be written back; %d already present would be left alone%s" % (
        r["missing"], r["present"], "; %d would be refused" % r["refused"] if r["refused"] else ""))
'
}

cmd_restore_untracked_locked() {
  local id="$CHECKPOINT_ID"
  local dir report
  dir="$(checkpoint_dir "$id")" || return 2
  if [ ! -d "$dir" ] || [ -L "$dir" ]; then
    echo "error: checkpoint not found or unsafe: $id" >&2
    return 2
  fi
  checkpoint_metadata_head "$dir" "$id" >/dev/null || return 1
  [ -f "$dir/untracked.tar" ] || {
    echo "error: checkpoint $id captured no untracked files (create it with --untracked)" >&2
    return 1
  }
  report="$(untracked_restore_report "$dir")" || return 1
  if [ "$AS_JSON" -eq 1 ]; then
    printf '{"schema":1,"id":"%s","untracked_only":true,"untracked":%s}\n' "$id" "$report"
  else
    untracked_restore_line "$report"
    [ "$APPLY" -eq 1 ] || echo "checkpoint: dry-run; pass --apply to write the missing files"
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
  current_head="$(git -C "$REPO" rev-parse HEAD 2>/dev/null)"
  [ "$current_head" = "$saved_head" ] || {
    echo "error: checkpoint $id belongs to HEAD $saved_head; current HEAD is $current_head" >&2
    return 1
  }
  ensure_supported_tree || return $?
  checkpoint_verify_raw "$id" || return $?

  local untracked_report=""
  if [ -f "$dir/untracked.tar" ] && [ "$APPLY" -eq 0 ]; then
    untracked_report="$(untracked_restore_report "$dir")" || return 1
  fi
  if [ "$APPLY" -eq 0 ]; then
    if [ "$AS_JSON" -eq 1 ] && [ -n "$untracked_report" ]; then
      printf '{"schema":1,"id":"%s","dry_run":true,"applicable":true,"untracked":%s}\n' "$id" "$untracked_report"
    elif [ "$AS_JSON" -eq 1 ]; then
      printf '{"schema":1,"id":"%s","dry_run":true,"applicable":true}\n' "$id"
    else
      echo "checkpoint: dry-run ok for $id (pass --apply to restore)"
      if [ -n "$untracked_report" ]; then
        untracked_restore_line "$untracked_report"
      else
        echo "checkpoint: untracked files will remain untouched"
      fi
    fi
    return 0
  fi

  backup_id="$(checkpoint_create_raw "auto-backup-before-$id")" || {
    echo "error: could not create the mandatory recovery checkpoint" >&2
    return 1
  }
  backup_dir="$CHECKPOINT_ROOT/$backup_id"
  if replace_tracked_state "$dir"; then
    if [ -f "$dir/untracked.tar" ]; then
      # Tracked state is back; the untracked copy only fills in what is missing.
      untracked_report="$(untracked_restore_report "$dir")" || {
        echo "error: tracked state was restored from $id, but its untracked files could not be written back" >&2
        return 1
      }
    fi
    if [ "$AS_JSON" -eq 1 ] && [ -n "$untracked_report" ]; then
      printf '{"schema":1,"id":"%s","applied":true,"backup_id":"%s","untracked":%s}\n' \
        "$id" "$backup_id" "$untracked_report"
    elif [ "$AS_JSON" -eq 1 ]; then
      printf '{"schema":1,"id":"%s","applied":true,"backup_id":"%s"}\n' \
        "$id" "$backup_id"
    else
      echo "checkpoint: restored $id"
      echo "checkpoint: recovery backup $backup_id"
      [ -z "$untracked_report" ] || untracked_restore_line "$untracked_report"
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
    --untracked) UNTRACKED=1; shift ;;
    --reuse) REUSE=1; shift ;;
    --untracked-only) UNTRACKED_ONLY=1; shift ;;
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
if [ "$ACTION" != create ]; then
  [ "$UNTRACKED" -eq 0 ] || fail "--untracked applies only to create"
  [ "$REUSE" -eq 0 ] || fail "--reuse applies only to create"
fi
[ "$ACTION" = restore ] || [ "$UNTRACKED_ONLY" -eq 0 ] || fail "--untracked-only applies only to restore"
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
    if [ "$UNTRACKED_ONLY" -eq 1 ]; then
      oms_with_file_lock "$CHECKPOINT_ROOT/.operation" cmd_restore_untracked_locked
    else
      oms_with_file_lock "$CHECKPOINT_ROOT/.operation" cmd_restore_locked
    fi
    ;;
esac
