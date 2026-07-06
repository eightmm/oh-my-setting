#!/usr/bin/env bash
set -euo pipefail

REPO_URL="${OH_MY_SETTING_REPO_URL:-https://github.com/eightmm/oh-my-setting.git}"
DEST="${OH_MY_SETTING_DIR:-$HOME/.oh-my-setting}"
GENERATE_SLURM="${OH_MY_SETTING_GENERATE_SLURM:-auto}"
GENERATE_MACHINE="${OH_MY_SETTING_GENERATE_MACHINE:-auto}"
INSTALL_TOOLS="${OH_MY_SETTING_INSTALL_TOOLS:-1}"
STAR_PROMPT="${OH_MY_SETTING_STAR_PROMPT:-1}"
AUTO_UPDATE="${OH_MY_SETTING_AUTO_UPDATE:-1}"

usage() {
  cat <<'EOF'
Usage: install.sh [--no-star] [--help]

Options:
  --no-star   Skip the GitHub star prompt.
  --help      Show this help.

Environment:
  OH_MY_SETTING_STAR_PROMPT=0      Skip the GitHub star prompt.
  OH_MY_SETTING_CLAUDE_HOOKS=0     Skip Claude Code hook registration.
  OH_MY_SETTING_GENERATE_MACHINE=0 Skip machine snapshot generation.
  OH_MY_SETTING_GENERATE_SLURM=0   Skip Slurm snapshot generation.
  OH_MY_SETTING_INSTALL_TOOLS=0    Skip Node/uv/agent CLI installation.
  OH_MY_SETTING_AUTO_UPDATE=0      Skip auto-update trigger installation.
  OH_MY_SETTING_AUTO_UPDATE_MODE=apply  Auto-apply fast-forward updates (default: check-only).
  OH_MY_SETTING_REQUIRE_TOOLS=0    Do not fail doctor on missing CLIs.
  OH_MY_SETTING_DIR=/path/to/dir   Install location.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --no-star)
      STAR_PROMPT=0
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

export OH_MY_SETTING_STAR_PROMPT="$STAR_PROMPT"

run_as_root() {
  if [ "$(id -u)" -eq 0 ]; then
    "$@"
  elif command -v sudo >/dev/null 2>&1; then
    sudo "$@"
  else
    echo "error: need root privileges for: $*" >&2
    exit 1
  fi
}

install_git_if_missing() {
  if command -v git >/dev/null 2>&1; then
    return 0
  fi

  echo "git missing; attempting install"

  if command -v apt-get >/dev/null 2>&1; then
    run_as_root apt-get update
    run_as_root apt-get install -y git
  elif command -v dnf >/dev/null 2>&1; then
    run_as_root dnf install -y git
  elif command -v yum >/dev/null 2>&1; then
    run_as_root yum install -y git
  elif command -v pacman >/dev/null 2>&1; then
    run_as_root pacman -Sy --noconfirm git
  elif command -v brew >/dev/null 2>&1; then
    brew install git
  else
    echo "error: git is required; install it manually and rerun" >&2
    exit 1
  fi
}

load_user_tool_paths() {
  export PATH="$HOME/.local/bin:$PATH"
  export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"

  if [ -s "$NVM_DIR/nvm.sh" ]; then
    # shellcheck disable=SC1091
    . "$NVM_DIR/nvm.sh"
    nvm use default >/dev/null 2>&1 || true
  fi
}

install_git_if_missing

if [ -d "$DEST/.git" ]; then
  git -C "$DEST" pull --ff-only
else
  mkdir -p "$(dirname "$DEST")"
  git clone "$REPO_URL" "$DEST"
fi

# Continue from the updated checkout so old piped/local installers do not run stale logic.
if [ "${OH_MY_SETTING_REEXECED:-0}" != "1" ] && [ -f "$DEST/install.sh" ]; then
  export OH_MY_SETTING_REEXECED=1
  exec bash "$DEST/install.sh"
fi

if [ "$INSTALL_TOOLS" = "1" ]; then
  "$DEST/scripts/install-tools.sh"
  load_user_tool_paths
elif [ "$INSTALL_TOOLS" = "0" ]; then
  echo "skipping tool install: OH_MY_SETTING_INSTALL_TOOLS=0"
  export OH_MY_SETTING_REQUIRE_TOOLS="${OH_MY_SETTING_REQUIRE_TOOLS:-0}"
  load_user_tool_paths
else
  echo "error: OH_MY_SETTING_INSTALL_TOOLS must be 0 or 1" >&2
  exit 2
fi

"$DEST/scripts/link.sh"

# Claude Code skill-router hook (deterministic skill suggestions at prompt
# time). Additive settings.json merge; Claude-only, non-fatal on failure.
if [ "${OH_MY_SETTING_CLAUDE_HOOKS:-1}" = "1" ]; then
  "$DEST/scripts/install-claude-hooks.sh" ||
    echo "warning: claude hook registration failed (install continues)" >&2
else
  echo "skipping claude hook registration: OH_MY_SETTING_CLAUDE_HOOKS=0"
fi

if [ "$GENERATE_SLURM" = "1" ] || { [ "$GENERATE_SLURM" = "auto" ] && command -v sinfo >/dev/null 2>&1; }; then
  "$DEST/scripts/generate-slurm-skill.sh"
fi

if [ "$GENERATE_MACHINE" != "0" ]; then
  "$DEST/scripts/write-machine-snapshot.sh"
fi

"$DEST/scripts/doctor.sh"

if [ "$AUTO_UPDATE" = "1" ]; then
  "$DEST/scripts/install-autoupdate.sh"
elif [ "$AUTO_UPDATE" = "0" ]; then
  echo "skipping auto-update trigger: OH_MY_SETTING_AUTO_UPDATE=0"
else
  echo "error: OH_MY_SETTING_AUTO_UPDATE must be 0 or 1" >&2
  exit 2
fi

if [ -s "${NVM_DIR:-$HOME/.nvm}/nvm.sh" ]; then
  cat <<'EOF'

If this shell still cannot find npm-installed CLIs, open a new shell or run:
  export NVM_DIR="$HOME/.nvm"
  [ -s "$NVM_DIR/nvm.sh" ] && . "$NVM_DIR/nvm.sh"
  nvm use default
EOF
fi

prompt_star_repo() {
  if [ "$STAR_PROMPT" = "0" ]; then
    return 0
  fi

  cat <<'EOF'

If oh-my-setting helped, please consider starring the repo:
  gh api --method PUT /user/starred/eightmm/oh-my-setting
EOF

  if ! command -v gh >/dev/null 2>&1; then
    return 0
  fi

  if ! gh auth status >/dev/null 2>&1; then
    echo "note: gh not authenticated; run 'gh auth login' to star with one command"
    return 0
  fi

  if [ ! -r /dev/tty ]; then
    return 0
  fi

  printf 'Star it now with gh? [y/N] ' >/dev/tty
  IFS= read -r answer </dev/tty || return 0

  case "$answer" in
    y|Y|yes|YES|Yes)
      if gh api --method PUT /user/starred/eightmm/oh-my-setting >/dev/null; then
        echo "ok: starred eightmm/oh-my-setting"
      else
        echo "warning: failed to star repo with gh api; you can run it manually later" >&2
      fi
      ;;
  esac
}

prompt_star_repo
