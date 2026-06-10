#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DRY_RUN=1
# shellcheck source=scripts/lib/agent-install-state.sh
. "$ROOT/scripts/lib/agent-install-state.sh"

usage() {
  cat <<'EOF'
Usage: cleanup.sh [--dry-run|--apply] [-h|--help]

Clean safe oh-my-setting install leftovers. Default is --dry-run.
Removes only known oh-my-setting legacy symlinks and backup skill symlinks;
never removes regular files, third-party plugins, caches, or directories.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    --apply)
      DRY_RUN=0
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

printf '# oh-my-setting cleanup\n\n'
if [ "$DRY_RUN" -eq 1 ]; then
  printf 'mode: dry-run\n\n'
else
  printf 'mode: apply\n\n'
fi

oms_ops_cleanup_legacy_links "$DRY_RUN"

if [ "$DRY_RUN" -eq 1 ]; then
  printf '\ncleanup: %s removable item(s) found\n' "$OMS_OPS_WOULD_REMOVE"
else
  printf '\ncleanup: removed %s item(s)\n' "$OMS_OPS_REMOVED"
  "$ROOT/scripts/skill-doctor.sh"
fi
