#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DRY_RUN="${OH_MY_SETTING_DRY_RUN:-0}"
ASSUME_YES="${OH_MY_SETTING_ASSUME_YES:-0}"
PURGE="${OH_MY_SETTING_PURGE:-0}"
PURGE_DIRTY="${OH_MY_SETTING_PURGE_DIRTY:-0}"

usage() {
  cat <<'EOF'
Usage: uninstall.sh [--yes] [--purge] [--purge-dirty] [--dry-run] [-h|--help]

Remove oh-my-setting managed links/copies (and restore backups when available). With
--purge also delete the oh-my-setting checkout directory itself.

Options:
  --yes       Assume yes for confirmation prompts.
  --purge     Also delete the checkout directory after unlinking.
  --purge-dirty
              With --purge, explicitly allow deletion of tracked or untracked
              checkout changes. Ignored runtime state is unaffected.
  --dry-run   Preview actions without making changes.

Environment:
  OH_MY_SETTING_ASSUME_YES=1  Same as --yes.
  OH_MY_SETTING_PURGE=1       Same as --purge.
  OH_MY_SETTING_PURGE_DIRTY=1 Same as --purge-dirty.
  OH_MY_SETTING_DRY_RUN=1     Same as --dry-run.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --yes|-y) ASSUME_YES=1; shift ;;
    --purge) PURGE=1; shift ;;
    --purge-dirty) PURGE_DIRTY=1; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *)
      echo "error: unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [ "$PURGE_DIRTY" = 1 ] && [ "$PURGE" != 1 ]; then
  echo "error: --purge-dirty requires --purge" >&2
  exit 2
fi

[ "${OMS_HARNESS_CHILD:-0}" != 1 ] || [ "$DRY_RUN" = 1 ] || {
  echo "error: a harness child cannot mutate OMS host lifecycle authority" >&2
  exit 2
}

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

# Preserve the standalone unsafe-purge guard before loading repo helpers: the
# guard must still protect a copied script whose checkout is incomplete.
# shellcheck source=scripts/lib/install-contract.sh
. "$ROOT/scripts/lib/install-contract.sh"
# shellcheck source=scripts/lib/install-lifecycle-lock.sh
. "$ROOT/scripts/lib/install-lifecycle-lock.sh"
uninstall_lifecycle_exit() {
  local code=$?

  trap - EXIT HUP INT TERM
  oms_install_lifecycle_lock_release
  exit "$code"
}
if [ "$DRY_RUN" != 1 ]; then
  trap uninstall_lifecycle_exit EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
  oms_install_lifecycle_lock_acquire uninstall || exit $?
fi
oms_install_require_owner "$ROOT" "uninstall" || exit 1

# Purge is the only uninstall mode that destroys the checkout itself. Resolve
# the complete dirty set before removing any integration so refusal is atomic:
# a protected local file must not leave the installation half-unlinked.
if [ "$PURGE" = 1 ] && [ "$PURGE_DIRTY" != 1 ] && [ -d "$ROOT/.git" ]; then
  purge_dirty="$(git -C "$ROOT" status --porcelain --untracked-files=all 2>/dev/null)" || {
    echo "error: cannot inspect checkout before purge: $ROOT" >&2
    exit 1
  }
  if [ -n "$purge_dirty" ]; then
    echo "error: refusing to purge a dirty checkout: $ROOT" >&2
    printf '%s\n' "$purge_dirty" | sed -n '1,20p' >&2
    echo "error: preserve the files, or rerun with --purge --purge-dirty" >&2
    exit 1
  fi
fi

removal_failed=0
run_removal() {  # LABEL COMMAND...
  local label="$1"
  shift
  if "$@"; then
    return 0
  fi
  echo "error: failed to remove $label" >&2
  removal_failed=1
  return 0
}

run_removal "the auto-update trigger" env OH_MY_SETTING_DRY_RUN="$DRY_RUN" \
  "$ROOT/scripts/uninstall-autoupdate.sh"
if [ "$DRY_RUN" = "1" ]; then
  echo "would remove claude oh-my-setting hooks/HUD from ~/.claude/settings.json"
  echo "would remove codex oh-my-setting plugin"
  echo "would unregister the oh-my-setting MCP server from claude and codex"
  echo "would uninstall the antigravity oh-my-setting plugin"
  echo "would revoke only Antigravity permissions added by oh-my-setting"
else
  run_removal "Claude hooks and HUD" "$ROOT/scripts/install-claude-hooks.sh" --remove
  run_removal "the Codex plugin" "$ROOT/scripts/install-codex-plugin.sh" --remove
  run_removal "MCP registrations" "$ROOT/scripts/install-mcp.sh" --remove
  run_removal "the Antigravity plugin" "$ROOT/scripts/install-agy-plugin.sh" --remove
  run_removal "managed Antigravity permissions" \
    "$ROOT/scripts/provider-permissions.sh" --remove
fi
if [ "$removal_failed" -ne 0 ]; then
  echo "error: uninstall stopped before unlink or purge; fix the errors above and retry" >&2
  exit 1
fi

purge_confirmed=0
if [ "$PURGE" = "1" ]; then
  if confirm "Delete checkout directory $ROOT?"; then
    purge_confirmed=1
  fi
fi

# Work Journal owns configuration outside the checkout. Remove it before
# unlinking the recovery command or receipt, so a failure leaves the install
# fully retryable and honors the same mutation boundary as other integrations.
if [ "$purge_confirmed" = "1" ]; then
  if [ "$DRY_RUN" = "1" ]; then
    echo "would remove managed Work Journal configuration"
  elif ! "$ROOT/scripts/journal.sh" disconnect --managed >/dev/null; then
    echo "error: managed Work Journal configuration was not removed" >&2
    echo "error: uninstall stopped before unlink or purge; fix the error above and retry" >&2
    exit 1
  fi
fi

OH_MY_SETTING_DRY_RUN="$DRY_RUN" "$ROOT/scripts/unlink.sh"

if [ "$PURGE" != "1" ]; then
  echo "uninstall: managed targets removed; checkout kept at $ROOT"
  echo "rerun with --purge to delete the checkout."
  exit 0
fi

if [ "$purge_confirmed" != "1" ]; then
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
