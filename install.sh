#!/usr/bin/env bash
set -euo pipefail

REPO_URL="${OH_MY_SETTING_REPO_URL:-https://github.com/eightmm/oh-my-setting.git}"
DEST="${OH_MY_SETTING_DIR:-$HOME/.oh-my-setting}"
INSTALL_TOOLS="${OH_MY_SETTING_INSTALL_TOOLS:-1}"
GENERATE_SLURM="${OH_MY_SETTING_GENERATE_SLURM:-auto}"
GENERATE_MACHINE="${OH_MY_SETTING_GENERATE_MACHINE:-auto}"

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

install_git_if_missing

if [ -d "$DEST/.git" ]; then
  git -C "$DEST" pull --ff-only
else
  mkdir -p "$(dirname "$DEST")"
  git clone "$REPO_URL" "$DEST"
fi

if [ "$INSTALL_TOOLS" != "0" ]; then
  "$DEST/scripts/install-tools.sh"
else
  export OH_MY_SETTING_REQUIRE_TOOLS=0
fi

"$DEST/scripts/link.sh"

if [ "$GENERATE_SLURM" = "1" ] || { [ "$GENERATE_SLURM" = "auto" ] && command -v sinfo >/dev/null 2>&1; }; then
  "$DEST/scripts/generate-slurm-skill.sh"
fi

if [ "$GENERATE_MACHINE" != "0" ]; then
  "$DEST/scripts/write-machine-snapshot.sh"
fi

"$DEST/scripts/doctor.sh"

prompt_star_repo() {
  cat <<'EOF'

If oh-my-setting helped, please consider starring the repo:
  gh api --method PUT /user/starred/eightmm/oh-my-setting
EOF

  if ! command -v gh >/dev/null 2>&1; then
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
