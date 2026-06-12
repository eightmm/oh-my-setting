#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DRY_RUN="${OH_MY_SETTING_DRY_RUN:-0}"
ASSUME_YES="${OH_MY_SETTING_ASSUME_YES:-0}"
PURGE="${OH_MY_SETTING_PURGE:-0}"

usage() {
  cat <<'EOF'
Usage: uninstall.sh [--yes] [--purge] [--dry-run] [-h|--help]

Remove oh-my-setting symlinks (and restore backups when available). With
--purge also delete the oh-my-setting checkout directory itself.

Options:
  --yes       Assume yes for confirmation prompts.
  --purge     Also delete the checkout directory after unlinking.
  --dry-run   Preview actions without making changes.

Environment:
  OH_MY_SETTING_ASSUME_YES=1  Same as --yes.
  OH_MY_SETTING_PURGE=1       Same as --purge.
  OH_MY_SETTING_DRY_RUN=1     Same as --dry-run.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --yes|-y) ASSUME_YES=1; shift ;;
    --purge) PURGE=1; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *)
      echo "error: unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

confirm() {
  local prompt="$1"
  if [ "$ASSUME_YES" = "1" ]; then
    return 0
  fi
  if [ ! -r /dev/tty ]; then
    echo "error: $prompt (no tty; rerun with --yes)" >&2
    exit 1
  fi
  printf '%s [y/N] ' "$prompt" >/dev/tty
  local answer
  IFS= read -r answer </dev/tty || answer=""
  case "$answer" in
    y|Y|yes|YES|Yes) return 0 ;;
    *) return 1 ;;
  esac
}

refuse_unsafe_purge_root() {
  case "$ROOT" in
    "$HOME"|/|"")
      echo "error: refusing to purge $ROOT" >&2
      exit 1
      ;;
  esac
}

if [ "$PURGE" = "1" ]; then
  refuse_unsafe_purge_root
fi

OH_MY_SETTING_DRY_RUN="$DRY_RUN" "$ROOT/scripts/uninstall-autoupdate.sh"
OH_MY_SETTING_DRY_RUN="$DRY_RUN" "$ROOT/scripts/unlink.sh"

if [ "$PURGE" != "1" ]; then
  echo "uninstall: symlinks removed; checkout kept at $ROOT"
  echo "rerun with --purge to delete the checkout."
  exit 0
fi

if ! confirm "Delete checkout directory $ROOT?"; then
  echo "purge: aborted"
  exit 0
fi

if [ "$DRY_RUN" = "1" ]; then
  echo "would remove $ROOT"
  exit 0
fi

# Defensive: refuse to purge the user's $HOME or unrelated paths.
refuse_unsafe_purge_root

cd /
rm -rf "$ROOT"
echo "purged $ROOT"
