#!/usr/bin/env bash
set -euo pipefail

# Scratch worktrees a parent session can add or remove while workers run. A
# plain `git worktree add` in the shared repository is a registration no worker
# guard can attribute, so it fails every running write worker; these carry the
# harness residue marker, which the guard's live-sibling exemption validates.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
# shellcheck source=scripts/lib/harness-residue.sh
. "$ROOT/scripts/lib/harness-residue.sh"

PREFIX=oh-my-setting-scratch

usage() {
  cat <<'EOF'
Usage: scratch-worktree.sh add [--repo PATH] [--base REV] [--owner-pid PID]
       scratch-worktree.sh remove [--repo PATH] [--force] PATH
       scratch-worktree.sh list [--repo PATH]

Create, remove and list detached scratch worktrees of a repository under the
delegate worktree root, each beside a residue marker. Running write workers
treat a live one as harness lifecycle instead of a Git metadata violation.

add prints the new worktree path. Its marker stays live while the owner
process runs: by default the nearest ancestor that is not a shell (the agent
session invoking it), or --owner-pid PID. After the owner exits the worktree is
ordinary harness residue that doctor/gc cleanup removes.
remove accepts only a worktree this command created; --force also discards
uncommitted changes in it.
Workers (OMS_HARNESS_CHILD=1 or a nonzero OMS_HARNESS_DELEGATE_DEPTH) are
refused: an exempt registration is parent authority.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

[ "$#" -gt 0 ] || { usage >&2; exit 2; }
ACTION="$1"
shift
case "$ACTION" in
  -h|--help) usage; exit 0 ;;
  add|remove|list) ;;
  *) fail "unknown action: $ACTION" ;;
esac

REPO=""
BASE=HEAD
OWNER_PID=""
FORCE=0
TARGET=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --base) [ "$ACTION" = add ] && [ "$#" -ge 2 ] || fail "--base requires add and a revision"; BASE="$2"; shift 2 ;;
    --owner-pid) [ "$ACTION" = add ] && [ "$#" -ge 2 ] || fail "--owner-pid requires add and a pid"; OWNER_PID="$2"; shift 2 ;;
    --force) [ "$ACTION" = remove ] || fail "--force applies to remove"; FORCE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    -*) fail "unknown option: $1" ;;
    *) [ "$ACTION" = remove ] && [ -z "$TARGET" ] || fail "unexpected argument: $1"; TARGET="$1"; shift ;;
  esac
done

if [ "${OMS_HARNESS_CHILD:-0}" = 1 ] || [ "${OMS_HARNESS_DELEGATE_DEPTH:-0}" != 0 ]; then
  fail "scratch-worktree is parent-only; a delegated worker must not create guard-exempt registrations"
fi

repo_root() {  # PATH -> physical top level
  local top
  top="$(git -C "$1" rev-parse --show-toplevel 2>/dev/null)" || return 1
  top="${top//$'\r'/}"
  oms_harness_physical_dir "$top"
}

ROOT_DIR="$(oms_harness_delegate_worktree_root)"
case "$ROOT_DIR" in /*) ;; *) fail "delegate worktree root must be an absolute path: $ROOT_DIR" ;; esac
REPO_PHYSICAL=""
if [ -n "$REPO" ] || [ "$ACTION" = add ]; then
  REPO_PHYSICAL="$(repo_root "${REPO:-$PWD}")" || fail "not a git repository: ${REPO:-$PWD}"
fi

# One process-table step, portable across procps/BSD ps and Cygwin/MSYS /proc.
parent_and_name() {  # PID -> "PPID NAME"
  local pid="$1" line="" ppid="" name=""
  line="$(ps -o ppid= -o comm= -p "$pid" 2>/dev/null | sed -n '1p')" || line=""
  line="${line//$'\r'/}"
  if [ -n "$line" ]; then
    # shellcheck disable=SC2086 # split "PPID NAME"
    set -- $line
    ppid="${1:-}"
    shift || true
    name="$*"
  elif [ -r "/proc/$pid/ppid" ]; then
    ppid="$(tr -d ' \r\n' < "/proc/$pid/ppid")"
    name="$(sed -n '1p' "/proc/$pid/exename" 2>/dev/null || true)"
  fi
  case "$ppid" in ""|*[!0-9]*) return 1 ;; esac
  name="${name##*/}"
  printf '%s %s\n' "$ppid" "${name#-}"
}

# A tool call's shell, and this script's dispatcher, exit with the command, so
# the owner is the nearest non-shell ancestor; without one, the outermost live
# shell, which still outlives the call.
default_owner_pid() {
  local pid="$PPID" step="" ppid="" name="" last="" hops=0

  while [ "$hops" -lt 16 ] && [ "$pid" -gt 1 ] 2>/dev/null; do
    kill -0 "$pid" 2>/dev/null || break
    step="$(parent_and_name "$pid")" || break
    ppid="${step%% *}"
    name="${step#* }"
    case "$name" in
      bash|sh|zsh|dash|ksh|mksh|fish|env|timeout|nohup|bash.exe|sh.exe|env.exe) last="$pid" ;;
      *) printf '%s\n' "$pid"; return 0 ;;
    esac
    pid="$ppid"
    hops=$((hops + 1))
  done
  [ -n "$last" ] || return 1
  printf '%s\n' "$last"
}

read_marker() {  # DIR -> sets M_KIND M_PID M_REPO M_WORKTREE M_TEMPORARY
  local marker="$1/.oh-my-setting-tmp"
  M_KIND="" M_PID="" M_REPO="" M_WORKTREE="" M_TEMPORARY=""
  [ -f "$marker" ] && [ ! -L "$marker" ] || return 1
  M_KIND="$(oms_harness_read_marker_value "$marker" kind)"
  M_PID="$(oms_harness_read_marker_value "$marker" pid)"
  M_REPO="$(oms_harness_read_marker_value "$marker" repo)"
  M_WORKTREE="$(oms_harness_read_marker_value "$marker" worktree)"
  M_TEMPORARY="$(oms_harness_read_marker_value "$marker" temporary)"
  [ "$M_KIND" = oh-my-setting-temp ] && [ "$M_TEMPORARY" = 1 ]
}

cmd_add() {
  local sha="" parent="" parent_physical="" worktree="" old_umask

  if [ -n "$OWNER_PID" ]; then
    case "$OWNER_PID" in *[!0-9]*) fail "--owner-pid must be a process id: $OWNER_PID" ;; esac
    [ "$OWNER_PID" != "$$" ] || fail "--owner-pid must outlive this command"
  else
    OWNER_PID="$(default_owner_pid)" ||
      fail "cannot find a session process to own the worktree; pass --owner-pid PID"
  fi
  kill -0 "$OWNER_PID" 2>/dev/null || fail "owner process is not running: $OWNER_PID"
  sha="$(git -C "$REPO_PHYSICAL" rev-parse --verify --quiet "$BASE^{commit}")" ||
    fail "not a commit: $BASE"
  sha="${sha//$'\r'/}"
  old_umask="$(umask)"
  umask 077
  mkdir -p "$ROOT_DIR" || { umask "$old_umask"; fail "could not create the delegate worktree root: $ROOT_DIR"; }
  umask "$old_umask"
  if [ -z "${OMS_DELEGATE_WORKTREE_ROOT:-}" ]; then
    [ ! -L "$ROOT_DIR" ] || fail "default delegate worktree root must not be a symbolic link: $ROOT_DIR"
    chmod 700 "$ROOT_DIR" || fail "could not make the delegate worktree root private: $ROOT_DIR"
  fi
  parent="$(mktemp -d "$ROOT_DIR/$PREFIX.XXXXXX")" || fail "mktemp failed in $ROOT_DIR"
  parent_physical="$(oms_harness_physical_dir "$parent")" || { rm -rf "$parent"; fail "cannot resolve $parent"; }
  worktree="$parent_physical/wt"
  # The marker precedes the registration: a worker guard capturing between the
  # two sees a pending entry whose live marker already vouches for it.
  oms_harness_mark_tmpdir "$parent_physical" "$REPO_PHYSICAL" "$worktree" "$OWNER_PID"
  if ! git -C "$REPO_PHYSICAL" -c core.fsmonitor=false worktree add --quiet --detach \
      "$worktree" "$sha" >&2; then
    rm -rf "$parent_physical"
    echo "error: git worktree add failed" >&2
    exit 1
  fi
  printf '%s\n' "$worktree"
}

cmd_remove() {
  local target="" parent="" parent_physical="" common="" entry="" gitfile="" worktree=""
  local -a remove_args

  [ -n "$TARGET" ] || fail "remove requires a scratch worktree path"
  target="${TARGET%/}"
  case "$target" in */wt) parent="${target%/wt}" ;; *) parent="$target" ;; esac
  parent_physical="$(oms_harness_physical_dir "$parent")" || fail "not a scratch worktree: $TARGET"
  case "$(basename "$parent_physical")" in "$PREFIX".*) ;; *) fail "not a scratch worktree: $TARGET" ;; esac
  [ "$(oms_harness_physical_dir "$ROOT_DIR" 2>/dev/null || true)" = "$(dirname "$parent_physical")" ] ||
    fail "not under the delegate worktree root: $TARGET"
  [ -O "$parent_physical" ] || fail "not owned by this user: $TARGET"
  read_marker "$parent_physical" || fail "no scratch marker beside: $TARGET"
  worktree="$parent_physical/wt"
  if [ -d "$worktree" ]; then
    oms_harness_safe_residue_worktree "$ROOT_DIR" "$parent_physical" "$M_REPO" "$M_WORKTREE" ||
      fail "marker and registration disagree; refusing: $TARGET"
    [ -z "$REPO_PHYSICAL" ] || [ "$REPO_PHYSICAL" = "$OMS_HARNESS_SAFE_RESIDUE_REPO" ] ||
      fail "scratch worktree belongs to another repository: $OMS_HARNESS_SAFE_RESIDUE_REPO"
    remove_args=(worktree remove)
    [ "$FORCE" = 0 ] || remove_args+=(--force)
    git -C "$OMS_HARNESS_SAFE_RESIDUE_REPO" "${remove_args[@]}" "$OMS_HARNESS_SAFE_RESIDUE_WORKTREE" >&2 ||
      { echo "error: git worktree remove failed (uncommitted changes need --force)" >&2; exit 1; }
  else
    # An interrupted removal left the checkout gone but its entry registered.
    # Drop that one entry, identified by its backpointer; prune would also take
    # other repositories' in-flight states.
    case "$M_WORKTREE" in /*/wt) ;; *) fail "marker does not describe: $TARGET" ;; esac
    [ "$(oms_harness_physical_dir "${M_WORKTREE%/wt}" 2>/dev/null || true)" = "$parent_physical" ] ||
      fail "marker does not describe: $TARGET"
    M_REPO="$(repo_root "$M_REPO")" || fail "marker repository is gone: $M_REPO"
    [ -z "$REPO_PHYSICAL" ] || [ "$REPO_PHYSICAL" = "$M_REPO" ] ||
      fail "scratch worktree belongs to another repository: $M_REPO"
    common="$(oms_harness_git_path_physical "$M_REPO" --git-common-dir)" || fail "cannot read $M_REPO"
    for entry in "$common"/worktrees/*; do
      [ -d "$entry" ] && [ ! -L "$entry" ] && [ -f "$entry/gitdir" ] || continue
      gitfile="$(sed -n '1p' "$entry/gitdir")"
      gitfile="${gitfile//$'\r'/}"
      [ "$gitfile" = "$worktree/.git" ] || [ "$gitfile" = "$M_WORKTREE/.git" ] || continue
      rm -rf "$entry"
    done
  fi
  # The marker goes last so every intermediate state above stays vouched for.
  rm -rf "$parent_physical"
  printf 'removed: %s\n' "$worktree"
}

cmd_list() {
  local dir=""
  [ -d "$ROOT_DIR" ] || return 0
  for dir in "$ROOT_DIR/$PREFIX".*; do
    [ -d "$dir" ] && [ ! -L "$dir" ] && [ -O "$dir" ] || continue
    read_marker "$dir" || continue
    case "$M_PID" in ""|*[!0-9]*) continue ;; esac
    kill -0 "$M_PID" 2>/dev/null || continue
    oms_harness_safe_residue_worktree "$ROOT_DIR" "$dir" "$M_REPO" "$M_WORKTREE" || continue
    [ -z "$REPO_PHYSICAL" ] || [ "$REPO_PHYSICAL" = "$OMS_HARNESS_SAFE_RESIDUE_REPO" ] || continue
    printf '%s\towner=%s\trepo=%s\n' "$OMS_HARNESS_SAFE_RESIDUE_WORKTREE" "$M_PID" \
      "$OMS_HARNESS_SAFE_RESIDUE_REPO"
  done
}

"cmd_$ACTION"
