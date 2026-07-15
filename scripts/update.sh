#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SKIP_TOOLS="${OH_MY_SETTING_UPDATE_SKIP_TOOLS:-1}"
SKIP_DOCTOR="${OH_MY_SETTING_UPDATE_SKIP_DOCTOR:-0}"
AUTO_UPDATE_SET="${OH_MY_SETTING_AUTO_UPDATE+x}"
CODEX_PLUGIN_SET="${OH_MY_SETTING_CODEX_PLUGIN+x}"
CLAUDE_HOOKS_SET="${OH_MY_SETTING_CLAUDE_HOOKS+x}"
MACHINE_SNAPSHOT_SET="${OH_MY_SETTING_GENERATE_MACHINE+x}"
SLURM_SNAPSHOT_SET="${OH_MY_SETTING_GENERATE_SLURM+x}"
AUTO_UPDATE="${OH_MY_SETTING_AUTO_UPDATE:-}"
CODEX_PLUGIN="${OH_MY_SETTING_CODEX_PLUGIN:-}"
CLAUDE_HOOKS="${OH_MY_SETTING_CLAUDE_HOOKS:-}"
MACHINE_SNAPSHOT="${OH_MY_SETTING_GENERATE_MACHINE:-}"
SLURM_SNAPSHOT="${OH_MY_SETTING_GENERATE_SLURM:-}"
INSTALL_REF_OVERRIDE=""
CHECK_ONLY=0
ROLLBACK=0
# shellcheck source=scripts/lib/install-contract.sh
. "$ROOT/scripts/lib/install-contract.sh"

usage() {
  cat <<'EOF'
Usage: update.sh [--check] [--rollback] [--ref REF] [--tools] [--no-tools] [--no-doctor] [-h|--help]

Update transactionally, preserving the installed component profile. Link or
doctor failure restores the previous commit, links, and install receipt.

Options:
  --check       Fetch and report whether the configured ref has changed.
  --rollback    Restore the previous successful commit from the receipt.
  --ref REF     Switch to an explicit branch, tag, or commit.
  --tools       Refresh Node/uv/provider tools after the core update commits.
  --no-tools    Skip tool refresh (default).
  --no-doctor   Skip the post-update doctor (disables its rollback gate).

Environment overrides persisted receipt components when explicitly set:
  OH_MY_SETTING_CLAUDE_HOOKS=0|1
  OH_MY_SETTING_CODEX_PLUGIN=0|1|auto
  OH_MY_SETTING_AUTO_UPDATE=0|1
  OH_MY_SETTING_GENERATE_MACHINE=0|1|auto
  OH_MY_SETTING_GENERATE_SLURM=0|1|auto
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --check) CHECK_ONLY=1; shift ;;
    --rollback) ROLLBACK=1; shift ;;
    --ref)
      [ "$#" -ge 2 ] || { echo "error: --ref requires a value" >&2; exit 2; }
      INSTALL_REF_OVERRIDE="$2"
      shift 2
      ;;
    --tools) SKIP_TOOLS=0; shift ;;
    --no-tools) SKIP_TOOLS=1; shift ;;
    --no-doctor) SKIP_DOCTOR=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
done

[ "$CHECK_ONLY" != "1" ] || [ "$ROLLBACK" != "1" ] || {
  echo "error: --check and --rollback cannot be combined" >&2
  exit 2
}
[ "$ROLLBACK" != "1" ] || [ -z "$INSTALL_REF_OVERRIDE" ] || {
  echo "error: --rollback and --ref cannot be combined" >&2
  exit 2
}

oms_install_require_owner "$ROOT" "update the install" || exit 1
[ -d "$ROOT/.git" ] || { echo "error: $ROOT is not a git checkout" >&2; exit 1; }

RECEIPT="$(oms_install_receipt_path)"
RECEIPT_SCHEMA="$(oms_install_receipt_field schema "$RECEIPT" 2>/dev/null || printf 1)"
INSTALL_REF="$(oms_install_receipt_field ref "$RECEIPT" 2>/dev/null || true)"
INSTALL_PROFILE="$(oms_install_receipt_field profile "$RECEIPT" 2>/dev/null || printf custom)"
RECEIPT_PREVIOUS_COMMIT="$(oms_install_receipt_field previous_commit "$RECEIPT" 2>/dev/null || true)"
[ -n "$INSTALL_REF" ] || {
  legacy_channel="$(oms_install_receipt_field channel "$RECEIPT" 2>/dev/null || true)"
  if [ "$legacy_channel" = "detached" ]; then
    INSTALL_REF="$(oms_install_receipt_field commit "$RECEIPT" 2>/dev/null || true)"
  elif [ -n "$legacy_channel" ]; then
    INSTALL_REF="$legacy_channel"
  else
    INSTALL_REF=edge
  fi
}
[ -z "$INSTALL_REF_OVERRIDE" ] || INSTALL_REF="$INSTALL_REF_OVERRIDE"

receipt_bool() {
  local key="$1"
  local fallback="$2"
  local value
  value="$(oms_install_receipt_field "components.$key" "$RECEIPT" 2>/dev/null || true)"
  case "$value" in true) printf 1 ;; false) printf 0 ;; *) printf '%s' "$fallback" ;; esac
}

legacy_claude_hooks() {
  [ -f "$HOME/.claude/settings.json" ] &&
    grep -Fq 'skill-router.sh' "$HOME/.claude/settings.json" 2>/dev/null && printf 1 || printf 0
}

legacy_codex_plugin() {
  if command -v codex >/dev/null 2>&1 &&
     codex plugin list --json 2>/dev/null |
       python3 -c 'import json,sys; d=json.load(sys.stdin); target="oh-my-setting@oh-my-setting-local"; sys.exit(0 if any(p.get("pluginId")==target and p.get("installed") for p in d.get("installed", [])) else 1)' 2>/dev/null; then
    printf 1
  else
    printf 0
  fi
}

legacy_auto_update() {
  local cron_file="${OH_MY_SETTING_AUTO_UPDATE_CRON_FILE:-}"
  if [ -e "$HOME/.config/systemd/user/oh-my-setting-autoupdate.timer" ] ||
     [ -e "$HOME/Library/LaunchAgents/com.oh-my-setting.autoupdate.plist" ]; then
    printf 1
  elif [ -n "$cron_file" ] && [ -f "$cron_file" ] &&
       grep -Fq '# oh-my-setting autoupdate:begin' "$cron_file"; then
    printf 1
  elif [ -z "$cron_file" ] && command -v crontab >/dev/null 2>&1 &&
       crontab -l 2>/dev/null | grep -Fq '# oh-my-setting autoupdate:begin'; then
    printf 1
  else
    printf 0
  fi
}

if [ "$RECEIPT_SCHEMA" = "2" ]; then
  PREVIOUS_CLAUDE_HOOKS="$(receipt_bool claude_hooks 0)"
  PREVIOUS_CODEX_PLUGIN="$(receipt_bool codex_plugin 0)"
  PREVIOUS_AUTO_UPDATE="$(receipt_bool auto_update 0)"
else
  PREVIOUS_CLAUDE_HOOKS="$(legacy_claude_hooks)"
  PREVIOUS_CODEX_PLUGIN="$(legacy_codex_plugin)"
  PREVIOUS_AUTO_UPDATE="$(legacy_auto_update)"
fi

if [ -z "$CLAUDE_HOOKS_SET" ]; then
  CLAUDE_HOOKS="$PREVIOUS_CLAUDE_HOOKS"
fi
if [ -z "$CODEX_PLUGIN_SET" ]; then
  CODEX_PLUGIN="$PREVIOUS_CODEX_PLUGIN"
fi
if [ -z "$AUTO_UPDATE_SET" ]; then
  AUTO_UPDATE="$PREVIOUS_AUTO_UPDATE"
fi

case "$CLAUDE_HOOKS:$AUTO_UPDATE" in
  [01]:[01]) ;;
  *) echo "error: hook and auto-update settings must be 0 or 1" >&2; exit 2 ;;
esac
case "$CODEX_PLUGIN" in
  auto) CODEX_PLUGIN="$(legacy_codex_plugin)" ;;
  0|1) ;;
  *) echo "error: OH_MY_SETTING_CODEX_PLUGIN must be 0, 1, or auto" >&2; exit 2 ;;
esac
case "$INSTALL_REF" in
  edge) ;;
  ""|-*|/*|*/|.*|*.|*..*|*//*|*/.*|*.lock|*.lock/*|*[!A-Za-z0-9._/-]*)
    echo "error: unsafe install ref: $INSTALL_REF" >&2
    exit 2
    ;;
esac

export OH_MY_SETTING_PROFILE="$INSTALL_PROFILE"
export OH_MY_SETTING_REF="$INSTALL_REF"
export OH_MY_SETTING_CLAUDE_HOOKS="$CLAUDE_HOOKS"
export OH_MY_SETTING_CODEX_PLUGIN="$CODEX_PLUGIN"
export OH_MY_SETTING_AUTO_UPDATE="$AUTO_UPDATE"
OH_MY_SETTING_INSTALL_TOOLS="$(receipt_bool tools 0)"
if [ -z "$MACHINE_SNAPSHOT_SET" ]; then
  MACHINE_SNAPSHOT="$(oms_install_receipt_mode machine_snapshot 0 "$RECEIPT")"
fi
if [ -z "$SLURM_SNAPSHOT_SET" ]; then
  SLURM_SNAPSHOT="$(oms_install_receipt_mode slurm_snapshot 0 "$RECEIPT")"
fi
OH_MY_SETTING_GENERATE_MACHINE="$MACHINE_SNAPSHOT"
OH_MY_SETTING_GENERATE_SLURM="$SLURM_SNAPSHOT"
export OH_MY_SETTING_INSTALL_TOOLS OH_MY_SETTING_GENERATE_MACHINE OH_MY_SETTING_GENERATE_SLURM
case "$OH_MY_SETTING_GENERATE_MACHINE:$OH_MY_SETTING_GENERATE_SLURM" in
  0:0|0:1|0:auto|1:0|1:1|1:auto|auto:0|auto:1|auto:auto) ;;
  *) echo "error: snapshot modes must be 0, 1, or auto" >&2; exit 2 ;;
esac

current="$(git -C "$ROOT" rev-parse HEAD)"
current_short="$(git -C "$ROOT" rev-parse --short HEAD)"
current_branch="$(git -C "$ROOT" symbolic-ref --quiet --short HEAD 2>/dev/null || true)"
edge_branch=""
echo "current: $current_short"

dirty="$(git -C "$ROOT" status --porcelain --untracked-files=normal)"

fetch_target() {
  if [ "$INSTALL_REF" = "edge" ]; then
    git -C "$ROOT" fetch origin
    git -C "$ROOT" remote set-head origin -a >/dev/null
    remote_head="$(git -C "$ROOT" symbolic-ref --quiet --short refs/remotes/origin/HEAD || true)"
    [ -n "$remote_head" ] || { echo "error: cannot resolve origin default branch" >&2; return 1; }
    edge_branch="${remote_head#origin/}"
    git -C "$ROOT" rev-parse "$remote_head^{commit}"
  else
    git -C "$ROOT" fetch --prune --tags origin
    target_ref="$(git -C "$ROOT" rev-parse --verify --quiet "refs/tags/$INSTALL_REF^{commit}" || true)"
    [ -n "$target_ref" ] || target_ref="$(git -C "$ROOT" rev-parse --verify --quiet "origin/$INSTALL_REF^{commit}" || true)"
    [ -n "$target_ref" ] || target_ref="$(git -C "$ROOT" rev-parse --verify --quiet "$INSTALL_REF^{commit}" || true)"
    [ -n "$target_ref" ] || { echo "error: cannot resolve install ref: $INSTALL_REF" >&2; return 1; }
    printf '%s\n' "$target_ref"
  fi
}

if [ "$ROLLBACK" = "1" ]; then
  target="$(oms_install_receipt_field previous_commit "$RECEIPT" 2>/dev/null || true)"
  case "$target" in
    [0-9a-fA-F][0-9a-fA-F]*) ;;
    *) echo "error: install receipt has no rollback commit" >&2; exit 1 ;;
  esac
  git -C "$ROOT" cat-file -e "$target^{commit}" 2>/dev/null || {
    echo "error: rollback commit is unavailable: $target" >&2
    exit 1
  }
else
  target="$(fetch_target)"
fi

if [ "$CHECK_ONLY" = "1" ]; then
  if [ "$current" = "$target" ]; then echo "update-check: up_to_date $current"; else echo "update-check: available $current -> $target"; fi
  exit 0
fi

if [ -n "$dirty" ]; then
  echo "error: refusing update or rollback from a dirty install checkout: $ROOT" >&2
  exit 1
fi

receipt_backup="$(mktemp "${TMPDIR:-/tmp}/oms-receipt.XXXXXX")"
if [ -f "$RECEIPT" ]; then cp "$RECEIPT" "$receipt_backup"; else : > "$receipt_backup"; fi
managed_backup="$(mktemp -d "${TMPDIR:-/tmp}/oms-managed.XXXXXX")"
MANAGED_PATHS=()
MANAGED_PRESENT=()

copy_preserving() {
  local source="$1"
  local target="$2"
  if [ -L "$source" ]; then
    ln -s "$(readlink "$source")" "$target"
  elif [ -d "$source" ]; then
    cp -R -P "$source" "$target"
  else
    cp -p "$source" "$target"
  fi
}

snapshot_managed_target() {
  local path="$1"
  local index="${#MANAGED_PATHS[@]}"
  local backup
  MANAGED_PATHS[$index]="$path"
  if [ -L "$path" ] || [ -e "$path" ]; then
    MANAGED_PRESENT[$index]=1
    copy_preserving "$path" "$managed_backup/$index"
  else
    MANAGED_PRESENT[$index]=0
  fi
  mkdir -p "$managed_backup/$index.backups"
  for backup in "$path".backup.*; do
    [ -L "$backup" ] || [ -e "$backup" ] || continue
    copy_preserving "$backup" "$managed_backup/$index.backups/${backup##*/}"
  done
}

restore_managed_targets() {
  local index=0
  local path
  local backup
  while [ "$index" -lt "${#MANAGED_PATHS[@]}" ]; do
    path="${MANAGED_PATHS[$index]}"
    for backup in "$path".backup.*; do
      [ -L "$backup" ] || [ -e "$backup" ] || continue
      rm -rf "$backup"
    done
    rm -rf "$path"
    if [ "${MANAGED_PRESENT[$index]}" = "1" ]; then
      mkdir -p "$(dirname "$path")"
      copy_preserving "$managed_backup/$index" "$path"
    fi
    for backup in "$managed_backup/$index.backups"/*; do
      [ -L "$backup" ] || [ -e "$backup" ] || continue
      copy_preserving "$backup" "$(dirname "$path")/${backup##*/}"
    done
    index=$((index + 1))
  done
}

for managed_path in \
  "$HOME/.codex/AGENTS.md" "$HOME/.claude/CLAUDE.md" "$HOME/.gemini/AGENTS.md" \
  "$HOME/.codex/skills" "$HOME/.claude/skills" "$HOME/.gemini/antigravity/skills" \
  "$HOME/.oh-my-setting-prompts" "$HOME/.oh-my-setting-workflows" \
  "$HOME/.local/bin/oms" "$HOME/.claude/settings.json" \
  "${OH_MY_SETTING_MACHINE_SNAPSHOT:-$ROOT/local/machine.md}" \
  "${OH_MY_SETTING_SLURM_REF:-$ROOT/custom-skills/slurm-hpc/references/cluster.generated.md}"; do
  snapshot_managed_target "$managed_path"
done

cleanup_update_tmp() { rm -f "$receipt_backup"; rm -rf "$managed_backup"; }
TRANSACTION_ACTIVE=0
trap cleanup_update_tmp EXIT

restore_previous() {
  local failed_target="$1"
  local rollback_failed=0
  echo "update: rolling back failed target $failed_target -> $current" >&2
  if [ -n "$current_branch" ]; then
    git -C "$ROOT" checkout "$current_branch" >/dev/null 2>&1 || true
    git -C "$ROOT" reset --hard "$current" >/dev/null
  else
    git -C "$ROOT" checkout --detach "$current" >/dev/null 2>&1
  fi
  if [ "$PREVIOUS_CODEX_PLUGIN" = "1" ]; then
    "$ROOT/scripts/install-codex-plugin.sh" >/dev/null 2>&1 || rollback_failed=1
  elif command -v codex >/dev/null 2>&1; then
    "$ROOT/scripts/install-codex-plugin.sh" --remove >/dev/null 2>&1 || rollback_failed=1
  fi
  restore_managed_targets || rollback_failed=1
  if [ -s "$receipt_backup" ]; then cp "$receipt_backup" "$RECEIPT"; else rm -f "$RECEIPT"; fi
  echo "update: rollback restored $current" >&2
  if [ "$rollback_failed" = "1" ]; then
    echo "error: rollback restored source and receipt but an external component needs repair" >&2
    return 1
  fi
}

handle_update_signal() {
  local signal="$1"
  local code=1
  case "$signal" in HUP) code=129 ;; INT) code=130 ;; TERM) code=143 ;; esac
  trap - HUP INT TERM
  if [ "$TRANSACTION_ACTIVE" = "1" ]; then
    restore_previous "signal:$signal" || true
  fi
  cleanup_update_tmp
  trap - EXIT
  exit "$code"
}
trap 'handle_update_signal HUP' HUP
trap 'handle_update_signal INT' INT
trap 'handle_update_signal TERM' TERM

reconcile_core() {
  if [ "$ROLLBACK" != 1 ] && [ "$current" = "$target" ]; then
    export OMS_INSTALL_PREVIOUS_COMMIT="$RECEIPT_PREVIOUS_COMMIT"
  else
    export OMS_INSTALL_PREVIOUS_COMMIT="$current"
  fi
  "$ROOT/scripts/link.sh" || return 1
  if [ "$CLAUDE_HOOKS" = "1" ]; then "$ROOT/scripts/install-claude-hooks.sh" || return 1; else "$ROOT/scripts/install-claude-hooks.sh" --remove || return 1; fi
  if [ "$CODEX_PLUGIN" = "1" ]; then
    "$ROOT/scripts/install-codex-plugin.sh" || return 1
  elif command -v codex >/dev/null 2>&1; then
    "$ROOT/scripts/install-codex-plugin.sh" --remove >/dev/null 2>&1 || return 1
  fi
  case "$OH_MY_SETTING_GENERATE_MACHINE" in
    1|auto) "$ROOT/scripts/write-machine-snapshot.sh" || return 1 ;;
  esac
  case "$OH_MY_SETTING_GENERATE_SLURM" in
    1) "$ROOT/scripts/generate-slurm-skill.sh" || return 1 ;;
    auto)
      if command -v sinfo >/dev/null 2>&1; then
        "$ROOT/scripts/generate-slurm-skill.sh" || return 1
      fi
      ;;
  esac
  if [ "$SKIP_DOCTOR" != "1" ]; then "$ROOT/scripts/doctor.sh" || return 1; fi
}

apply_target() {
  if [ "$INSTALL_REF" != "edge" ] || [ "$ROLLBACK" = "1" ]; then
    git -C "$ROOT" checkout --detach "$target" >/dev/null
    return
  fi
  remote_head="$(git -C "$ROOT" symbolic-ref --quiet --short refs/remotes/origin/HEAD)"
  edge_branch="${remote_head#origin/}"
  if git -C "$ROOT" show-ref --verify --quiet "refs/heads/$edge_branch"; then
    git -C "$ROOT" checkout "$edge_branch"
  else
    git -C "$ROOT" checkout -b "$edge_branch" --track "origin/$edge_branch"
  fi
  git -C "$ROOT" merge --ff-only "$target"
}

TRANSACTION_ACTIVE=1
if ! apply_target; then
  restore_previous "$target" || true
  exit 1
fi

if ! reconcile_core; then
  restore_previous "$target"
  exit 1
fi
TRANSACTION_ACTIVE=0

if [ "$SKIP_TOOLS" != "1" ]; then
  "$ROOT/scripts/install-tools.sh" --upgrade
fi
if [ "$AUTO_UPDATE" = "1" ]; then
  "$ROOT/scripts/install-autoupdate.sh"
else
  "$ROOT/scripts/uninstall-autoupdate.sh" >/dev/null
fi

new="$(git -C "$ROOT" rev-parse HEAD)"
if [ "$current" = "$new" ]; then echo "already up to date: ${new:0:7}"; else echo "updated: ${current:0:7} -> ${new:0:7}"; fi
echo "update: ok"
