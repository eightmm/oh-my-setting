#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
ROOT="$(cd "$ROOT" && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"
# shellcheck source=scripts/lib/agent-task-common.sh
. "$ROOT/scripts/lib/agent-task-common.sh"

REPO="$PWD"
STATE_FILE=""
STRICT=0
FROM_TASK=0
ACTION=""
declare -a ALLOW_PATHS=()
declare -a DENY_PATHS=()

usage() {
  cat <<'EOF'
Usage: change-guard.sh [options] <begin|check|end|status>

Advisory guard for live agent edits. It snapshots the current dirty workspace,
then warns if pre-existing dirty files were changed or the current diff escapes
the declared path scope. Exit is 0 by default; pass --strict to make warnings
fail.

Options:
  --repo PATH       Git repo. Default: PWD.
  --file PATH       State file. Default: REPO/.oms/guards/change-guard.tsv.
  --allow PATH      Allowed changed path prefix or glob. Repeatable.
  --deny PATH       Forbidden changed path prefix or glob. Repeatable.
                    Deny beats allow: a denied path warns even if also allowed.
  --from-task       Also read allowed_paths and forbidden_paths from the active
                    task Constraints.
  --strict          Exit 1 when check finds warnings.
  -h, --help        Show help.

Task scope format:
  agent-task.sh --repo . update --constraint "allowed_paths: scripts/, README.md"
  agent-task.sh --repo . update --constraint "forbidden_paths: scripts/lib/, *.lock"
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

sha_for_path() {
  local path="$1"
  if [ -f "$REPO/$path" ]; then
    if command -v sha256sum >/dev/null 2>&1; then
      sha256sum "$REPO/$path" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
      shasum -a 256 "$REPO/$path" | awk '{print $1}'
    else
      wc -c < "$REPO/$path" | tr -d ' '
    fi
  elif [ -e "$REPO/$path" ]; then
    printf '<non-regular>'
  else
    printf '<missing>'
  fi
}

normalize_path() {
  local path="$1"
  path="${path#./}"
  path="${path%/}"
  printf '%s\n' "$path"
}

parse_task_paths() {
  # $1: constraint key (allowed_paths | forbidden_paths)
  local key="$1"
  local task_file
  task_file="$(agent_task_project_file "$REPO")" || return 0
  [ -s "$task_file" ] || return 0
  awk -v key="$key" '
    /^## Constraints$/ { in_section = 1; next }
    in_section == 1 && /^## / { in_section = 0 }
    in_section == 1 {
      line = $0
      sub(/^- [^[]*\[[^]]*\] /, "", line)
      if (line ~ ("(^|[[:space:]])" key ":[[:space:]]*")) {
        sub("^.*" key ":[[:space:]]*", "", line)
        gsub(/,/, " ", line)
        print line
      }
    }
  ' "$task_file" |
    tr ' ' '\n' |
    sed '/^$/d'
}

changed_paths() {
  {
    git -C "$REPO" diff --name-only HEAD -- 2>/dev/null || git -C "$REPO" diff --name-only -- 2>/dev/null || true
    git -C "$REPO" ls-files --others --exclude-standard 2>/dev/null || true
  } |
    sed '/^\.oms\//d' |
    sort -u
}

dirty_paths() {
  git -C "$REPO" status --porcelain=v1 --untracked-files=all -- 2>/dev/null |
    sed '/^.. \.oms\//d' |
    sed -E 's/^.. //; s/^.* -> //'
}

path_allowed() {
  local path="$1"
  local allow
  [ "${#ALLOW_PATHS[@]}" -gt 0 ] || return 0
  for allow in "${ALLOW_PATHS[@]}"; do
    [ -n "$allow" ] || continue
    case "$allow" in
      *'*'*|*'?'*|*'['*)
        # shellcheck disable=SC2254
        # Intentional: --allow accepts shell-style globs for changed paths.
        case "$path" in
          $allow) return 0 ;;
        esac
        ;;
      *)
        if [ "$path" = "$allow" ] || [ "${path#"$allow"/}" != "$path" ]; then
          return 0
        fi
        ;;
    esac
  done
  return 1
}

path_denied() {
  local path="$1"
  local deny
  for deny in "${DENY_PATHS[@]:-}"; do
    [ -n "$deny" ] || continue
    case "$deny" in
      *'*'*|*'?'*|*'['*)
        # shellcheck disable=SC2254
        case "$path" in
          $deny) return 0 ;;
        esac
        ;;
      *)
        if [ "$path" = "$deny" ] || [ "${path#"$deny"/}" != "$path" ]; then
          return 0
        fi
        ;;
    esac
  done
  return 1
}

load_state() {
  [ -f "$STATE_FILE" ] || fail "state file not found: $STATE_FILE"
  ALLOW_PATHS=()
  DENY_PATHS=()
  while IFS=$'\t' read -r kind a _rest; do
    case "$kind" in
      repo) REPO="$a" ;;
      allow) ALLOW_PATHS+=("$a") ;;
      deny) DENY_PATHS+=("$a") ;;
      dirty) : ;;
    esac
  done < "$STATE_FILE"
}

cmd_begin() {
  local path allow
  mkdir -p "$(dirname "$STATE_FILE")"
  agent_memory_ensure_oms_ignore_for_path "$STATE_FILE" 2>/dev/null || true
  if [ "$FROM_TASK" -eq 1 ]; then
    while IFS= read -r allow; do
      [ -n "$allow" ] && ALLOW_PATHS+=("$(normalize_path "$allow")")
    done <<EOF
$(parse_task_paths allowed_paths)
EOF
    while IFS= read -r deny; do
      [ -n "$deny" ] && DENY_PATHS+=("$(normalize_path "$deny")")
    done <<EOF
$(parse_task_paths forbidden_paths)
EOF
  fi
  {
    printf 'repo\t%s\n' "$REPO"
    printf 'head\t%s\n' "$(git -C "$REPO" rev-parse --verify HEAD 2>/dev/null || printf 'no-head')"
    for allow in "${ALLOW_PATHS[@]}"; do
      printf 'allow\t%s\n' "$(normalize_path "$allow")"
    done
    for deny in "${DENY_PATHS[@]:-}"; do
      [ -n "$deny" ] && printf 'deny\t%s\n' "$(normalize_path "$deny")"
    done
    while IFS= read -r path; do
      [ -n "$path" ] || continue
      path="$(normalize_path "$path")"
      printf 'dirty\t%s\t%s\n' "$path" "$(sha_for_path "$path")"
    done <<EOF
$(dirty_paths)
EOF
  } > "$STATE_FILE"
  echo "change-guard: snapshot $STATE_FILE"
}

cmd_check() {
  local warnings=0
  local path old_sha new_sha

  load_state
  while IFS=$'\t' read -r kind path old_sha; do
    [ "$kind" = "dirty" ] || continue
    new_sha="$(sha_for_path "$path")"
    if [ "$new_sha" != "$old_sha" ]; then
      printf 'warning: pre-existing dirty file changed since guard begin: %s\n' "$path"
      warnings=$((warnings + 1))
    fi
  done < "$STATE_FILE"

  while IFS= read -r path; do
    [ -n "$path" ] || continue
    path="$(normalize_path "$path")"
    if path_denied "$path"; then
      printf 'warning: changed path in forbidden scope: %s\n' "$path"
      warnings=$((warnings + 1))
    elif ! path_allowed "$path"; then
      printf 'warning: changed path outside declared scope: %s\n' "$path"
      warnings=$((warnings + 1))
    fi
  done <<EOF
$(changed_paths)
EOF

  if [ "$warnings" -eq 0 ]; then
    echo "change-guard: ok"
  else
    echo "change-guard: $warnings warning(s)"
  fi
  [ "$STRICT" -eq 0 ] || [ "$warnings" -eq 0 ]
}

cmd_end() {
  rm -f "$STATE_FILE"
  echo "change-guard: ended"
}

cmd_status() {
  if [ -f "$STATE_FILE" ]; then
    echo "change-guard: active ($STATE_FILE)"
  else
    echo "change-guard: inactive ($STATE_FILE)"
  fi
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo)
      [ "$#" -ge 2 ] || fail "--repo requires path"
      REPO="$2"
      shift 2
      ;;
    --file)
      [ "$#" -ge 2 ] || fail "--file requires path"
      STATE_FILE="$2"
      shift 2
      ;;
    --allow)
      [ "$#" -ge 2 ] || fail "--allow requires path"
      ALLOW_PATHS+=("$(normalize_path "$2")")
      shift 2
      ;;
    --deny)
      [ "$#" -ge 2 ] || fail "--deny requires path"
      DENY_PATHS+=("$(normalize_path "$2")")
      shift 2
      ;;
    --from-task)
      FROM_TASK=1
      shift
      ;;
    --strict)
      STRICT=1
      shift
      ;;
    begin|check|end|status)
      ACTION="$1"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

[ -n "$ACTION" ] || { usage >&2; exit 2; }
REPO="$(cd "$REPO" && pwd)" || fail "bad --repo"
git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || fail "not a git repo: $REPO"
STATE_FILE="${STATE_FILE:-$REPO/.oms/guards/change-guard.tsv}"

case "$ACTION" in
  begin) cmd_begin ;;
  check) cmd_check ;;
  end) cmd_end ;;
  status) cmd_status ;;
esac
