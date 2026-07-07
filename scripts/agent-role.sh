#!/usr/bin/env bash
set -euo pipefail

# Named, reusable role profiles shared by the three agent CLIs. A role is a
# small markdown file describing how a worker should act (a reviewer, a
# refactorer, a test-writer). It is DATA, not an orchestrator: the owning agent
# picks a role and injects it into a delegated worker's brief
# (multi-agent-delegate.sh --role NAME) or an agent-plan task's role field.
# Repo roles live in .oms/roles/<name>.md; a global fallback lives under
# ~/.oh-my-setting/local/roles so a role can be reused across repos.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"

REPO="$PWD"
NAME=""
SCOPE="project"
ACTION=""

usage() {
  cat <<'EOF'
Usage: agent-role.sh [options] <list|show|path|resolve|init>

Manage named worker role profiles (.oms/roles/<name>.md; global fallback
~/.oh-my-setting/local/roles/<name>.md). Roles are injected into a delegated
worker brief with multi-agent-delegate.sh --role NAME.

Options:
  --repo PATH   Repo for project-scoped roles (default: PWD, git-root anchored).
  --name NAME   Role name ([A-Za-z0-9._-]+). Required for show/path/resolve/init.
  --global      Use the global role store instead of the repo.
  -h, --help    Show help.

Commands:
  list      List available role names (repo then global; deduped).
  show      Print the resolved role file (repo first, then global).
  path      Print where a role file lives for the chosen scope (may not exist).
  resolve   Print the first existing role file path (repo then global);
            exit 3 if none. Used by multi-agent-delegate.sh --role.
  init      Create a starter role file at the chosen scope if missing.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

roles_dir_project() {
  local repo
  repo="$(oms_repo_root "$1")" || return 1
  printf '%s/.oms/roles\n' "$repo"
}

roles_dir_global() {
  printf '%s\n' "${OH_MY_SETTING_ROLES_DIR:-$HOME/.oh-my-setting/local/roles}"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --name) [ "$#" -ge 2 ] || fail "--name requires a value"; NAME="$2"; shift 2 ;;
    --global) SCOPE="global"; shift ;;
    -h|--help) usage; exit 0 ;;
    list|show|path|resolve|init) ACTION="$1"; shift ;;
    *) fail "unknown argument: $1" ;;
  esac
done

ACTION="${ACTION:-list}"

case "$ACTION" in
  show|path|resolve|init)
    [ -n "$NAME" ] || fail "$ACTION requires --name"
    case "$NAME" in
      *[!A-Za-z0-9._-]*|"") fail "--name must match [A-Za-z0-9._-]+" ;;
    esac
    ;;
esac

proj_dir="$(roles_dir_project "$REPO" 2>/dev/null || true)"
glob_dir="$(roles_dir_global)"

case "$ACTION" in
  list)
    {
      [ -d "$proj_dir" ] && find "$proj_dir" -maxdepth 1 -type f -name '*.md' 2>/dev/null
      [ -d "$glob_dir" ] && find "$glob_dir" -maxdepth 1 -type f -name '*.md' 2>/dev/null
      true
    } | while IFS= read -r f; do
      [ -n "$f" ] || continue
      b="$(basename "$f")"
      printf '%s\n' "${b%.md}"
    done | sort -u
    ;;
  path)
    if [ "$SCOPE" = "global" ]; then
      printf '%s/%s.md\n' "$glob_dir" "$NAME"
    else
      [ -n "$proj_dir" ] || fail "could not resolve repo for project scope"
      printf '%s/%s.md\n' "$proj_dir" "$NAME"
    fi
    ;;
  resolve|show)
    resolved=""
    [ -n "$proj_dir" ] && [ -f "$proj_dir/$NAME.md" ] && resolved="$proj_dir/$NAME.md"
    [ -z "$resolved" ] && [ -f "$glob_dir/$NAME.md" ] && resolved="$glob_dir/$NAME.md"
    [ -n "$resolved" ] || { echo "error: no role '$NAME' (looked in $proj_dir and $glob_dir)" >&2; exit 3; }
    if [ "$ACTION" = "resolve" ]; then
      printf '%s\n' "$resolved"
    else
      cat "$resolved"
    fi
    ;;
  init)
    if [ "$SCOPE" = "global" ]; then
      dir="$glob_dir"
    else
      [ -n "$proj_dir" ] || fail "could not resolve repo for project scope"
      dir="$proj_dir"
      agent_memory_ensure_oms_ignore_for_path "$dir" 2>/dev/null || true
    fi
    target="$dir/$NAME.md"
    [ -e "$target" ] && fail "role already exists: $target"
    mkdir -p "$dir"
    {
      printf '# Role: %s\n\n' "$NAME"
      printf 'You are acting as the "%s" role for this task.\n\n' "$NAME"
      printf '## Mandate\n\n- Describe what this role does and does not do.\n\n'
      printf '## Rules\n\n- One actionable rule per line.\n\n'
      printf '## Output\n\n- State what the role must produce.\n'
    } > "$target"
    echo "role: created $target"
    ;;
  *)
    fail "unknown command: $ACTION"
    ;;
esac
