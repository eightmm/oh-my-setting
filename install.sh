#!/usr/bin/env bash
set -euo pipefail

REPO_URL="${OH_MY_SETTING_REPO_URL:-https://github.com/eightmm/oh-my-setting.git}"
DEST="${OH_MY_SETTING_DIR:-$HOME/.oh-my-setting}"
INSTALLER_DEFAULT_REF="edge"
REF="${OH_MY_SETTING_REF:-$INSTALLER_DEFAULT_REF}"
PROFILE="${OH_MY_SETTING_PROFILE:-minimal}"
GENERATE_SLURM="${OH_MY_SETTING_GENERATE_SLURM:-0}"
GENERATE_MACHINE="${OH_MY_SETTING_GENERATE_MACHINE:-0}"
# The three provider CLIs, gh, and ntn are what the harness is for: a council with one
# installed peer is not a council, and github-source/ci-status have nothing to
# talk to without gh. Installing them is the default, not an opt-in flag nobody
# passes. --no-tools remains for a machine that cannot or must not have them.
INSTALL_TOOLS="${OH_MY_SETTING_INSTALL_TOOLS:-1}"
CONNECT_SERVICES="${OH_MY_SETTING_CONNECT_SERVICES:-auto}"
STAR_PROMPT="${OH_MY_SETTING_STAR_PROMPT:-0}"
# The apply-mode update trigger is a default, not an opt-in: a harness that
# silently goes stale stops matching the docs and skills its agents trust.
# Apply fast-forwards a clean checkout on the daily schedule; recording
# without touching stays available (OH_MY_SETTING_AUTO_UPDATE_MODE=check),
# and --no-auto-update opts out.
AUTO_UPDATE="${OH_MY_SETTING_AUTO_UPDATE:-1}"
CODEX_PLUGIN="${OH_MY_SETTING_CODEX_PLUGIN:-auto}"
PEER_PERMISSIONS="${OH_MY_SETTING_PEER_PERMISSIONS:-0}"
NOTION_DATA_SOURCE_ID="${OH_MY_SETTING_NOTION_DATA_SOURCE_ID:-${OMS_WORK_JOURNAL_NOTION_DATA_SOURCE_ID:-}}"

usage() {
  cat <<'EOF'
Usage: install.sh [--ref REF] [--full] [--no-tools] [--connect-services] [--no-connect-services] [--no-auto-update] [--machine-snapshot] [--slurm-snapshot] [--notion-data-source ID] [--peer-permissions] [--star] [--help]

Options:
  --ref REF           Install edge, a tag, branch, or commit (default: installer channel).
  --full              Install provider tools, machine snapshot, and update timer.
  --tools             Install Node, uv, provider CLIs, gh, and ntn (already the default).
  --no-tools          Skip them; doctor then treats every provider CLI as optional.
  --connect-services  Require interactive gh and Notion login plus journal linking.
  --no-connect-services
                      Skip account login and automatic journal discovery.
  --auto-update       Install the apply-mode update timer (already the default).
  --no-auto-update    Skip the auto-update trigger.
  --machine-snapshot  Generate local machine metadata.
  --slurm-snapshot    Generate local Slurm cluster metadata when available.
  --notion-data-source ID
                      Select the Work Journal Notion mirror. Authentication is
                      owned by ntn and is never persisted by oh-my-setting.
  --peer-permissions  Grant Antigravity the standing consult permissions
                      (read_file(*), command(*)) so headless council calls are
                      not auto-denied. Writes the user's Antigravity settings
                      with a .bak beside it. Without this flag the installer
                      only reports what a headless peer would be denied.
  --star              Offer the optional GitHub star prompt.
  --no-star           Skip the star prompt (compatibility; default).
  --help              Show this help.

Environment:
  OH_MY_SETTING_STAR_PROMPT=1      Enable the GitHub star prompt.
  OH_MY_SETTING_REF=edge|REF       Track edge or pin an exact Git ref.
  OH_MY_SETTING_PROFILE=NAME       Receipt profile: minimal, full, or custom.
  OH_MY_SETTING_CLAUDE_HOOKS=0     Skip Claude Code hooks and usage HUD.
  OH_MY_SETTING_CODEX_PLUGIN=0|1|auto  Skip, require, or auto-detect Codex plugin setup.
  OH_MY_SETTING_PEER_PERMISSIONS=1 Grant Antigravity consult permissions.
  OH_MY_SETTING_GENERATE_MACHINE=1 Generate a machine snapshot.
  OH_MY_SETTING_GENERATE_SLURM=1   Generate a Slurm snapshot.
  OH_MY_SETTING_INSTALL_TOOLS=0    Skip the Node/uv/provider CLI/gh/ntn install (default: 1).
  OH_MY_SETTING_CONNECT_SERVICES=auto|required|0
                                   Auto-connect in an interactive terminal,
                                   require connection, or skip it (default: auto).
  OH_MY_SETTING_AUTO_UPDATE=0      Skip the auto-update trigger (default: 1).
  OH_MY_SETTING_NOTION_DATA_SOURCE_ID=ID
                                   Configure the Work Journal Notion mirror.
  OH_MY_SETTING_AUTO_UPDATE_MODE=check  Only record available updates (default: apply).
  OH_MY_SETTING_REQUIRE_TOOLS=0    Let doctor treat a missing CLI as optional
                                   (default: 1 whenever the tools were installed).
  OH_MY_SETTING_LINK_MODE=MODE     auto, symlink, or copy. auto uses copies on
                                   Windows Git Bash and symlinks elsewhere.
  OH_MY_SETTING_DIR=/path/to/dir   Install location.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --ref)
      [ "$#" -ge 2 ] || { echo "error: --ref requires a value" >&2; exit 2; }
      REF="$2"
      shift
      ;;
    --full)
      PROFILE=full
      INSTALL_TOOLS=1
      GENERATE_MACHINE=auto
      GENERATE_SLURM=auto
      AUTO_UPDATE=1
      ;;
    --tools)
      [ "$PROFILE" = "full" ] || PROFILE=custom
      INSTALL_TOOLS=1
      ;;
    --no-tools)
      [ "$PROFILE" = "full" ] || PROFILE=custom
      INSTALL_TOOLS=0
      ;;
    --connect-services)
      [ "$PROFILE" = "full" ] || PROFILE=custom
      CONNECT_SERVICES=required
      ;;
    --no-connect-services)
      [ "$PROFILE" = "full" ] || PROFILE=custom
      CONNECT_SERVICES=0
      ;;
    --auto-update)
      [ "$PROFILE" = "full" ] || PROFILE=custom
      AUTO_UPDATE=1
      ;;
    --no-auto-update)
      [ "$PROFILE" = "full" ] || PROFILE=custom
      AUTO_UPDATE=0
      ;;
    --machine-snapshot)
      [ "$PROFILE" = "full" ] || PROFILE=custom
      GENERATE_MACHINE=1
      ;;
    --slurm-snapshot)
      [ "$PROFILE" = "full" ] || PROFILE=custom
      GENERATE_SLURM=1
      ;;
    --notion-data-source)
      [ "$#" -ge 2 ] || {
        echo "error: --notion-data-source requires a value" >&2
        exit 2
      }
      [ "$PROFILE" = "full" ] || PROFILE=custom
      NOTION_DATA_SOURCE_ID="$2"
      shift
      ;;
    --peer-permissions)
      [ "$PROFILE" = "full" ] || PROFILE=custom
      PEER_PERMISSIONS=1
      ;;
    --star)
      [ "$PROFILE" = "full" ] || PROFILE=custom
      STAR_PROMPT=1
      ;;
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

case "$PROFILE" in
  minimal|full|custom) ;;
  *) echo "error: OH_MY_SETTING_PROFILE must be minimal, full, or custom" >&2; exit 2 ;;
esac
case "$PEER_PERMISSIONS" in
  0|1) ;;
  *) echo "error: OH_MY_SETTING_PEER_PERMISSIONS must be 0 or 1" >&2; exit 2 ;;
esac
case "$CONNECT_SERVICES" in
  1) CONNECT_SERVICES=required ;;
  auto|required|0) ;;
  *)
    echo "error: OH_MY_SETTING_CONNECT_SERVICES must be auto, required, or 0" >&2
    exit 2
    ;;
esac
case "$GENERATE_MACHINE:$GENERATE_SLURM" in
  0:0|0:1|0:auto|1:0|1:1|1:auto|auto:0|auto:1|auto:auto) ;;
  *) echo "error: snapshot modes must be 0, 1, or auto" >&2; exit 2 ;;
esac
case "$REF" in
  edge) ;;
  ""|-*|/*|*/|.*|*.|*..*|*//*|*/.*|*.lock|*.lock/*|*[!A-Za-z0-9._/-]*)
    echo "error: unsafe OH_MY_SETTING_REF: $REF" >&2
    exit 2
    ;;
esac

export OH_MY_SETTING_REF="$REF"
export OH_MY_SETTING_PROFILE="$PROFILE"
export OH_MY_SETTING_STAR_PROMPT="$STAR_PROMPT"
export OH_MY_SETTING_INSTALL_TOOLS="$INSTALL_TOOLS"
export OH_MY_SETTING_CONNECT_SERVICES="$CONNECT_SERVICES"
export OH_MY_SETTING_GENERATE_MACHINE="$GENERATE_MACHINE"
export OH_MY_SETTING_GENERATE_SLURM="$GENERATE_SLURM"
export OH_MY_SETTING_AUTO_UPDATE="$AUTO_UPDATE"
export OH_MY_SETTING_CODEX_PLUGIN="$CODEX_PLUGIN"
export OH_MY_SETTING_PEER_PERMISSIONS="$PEER_PERMISSIONS"
export OH_MY_SETTING_NOTION_DATA_SOURCE_ID="$NOTION_DATA_SOURCE_ID"

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

ensure_python3() {
  local candidate=""
  local uv_interp=""
  local shim="$HOME/.local/bin/python3"

  export PATH="$HOME/.local/bin:$PATH"
  if command -v python3 >/dev/null 2>&1 &&
     python3 -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 9) else 1)' 2>/dev/null; then
    return 0
  fi
  if oms_platform_is_windows; then
    if command -v python >/dev/null 2>&1 &&
       python -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 9) else 1)' 2>/dev/null; then
      candidate=python
    elif command -v py >/dev/null 2>&1 &&
         py -3 -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 9) else 1)' 2>/dev/null; then
      candidate=py
    fi
  fi
  # uv-only machines (fresh workstations, cluster accounts) carry a managed
  # CPython without exposing any python3 command. Adopt it through a managed
  # shim that resolves `uv python find` at call time, so the shim survives uv
  # relocating or upgrading its interpreters.
  if [ -z "$candidate" ] && command -v uv >/dev/null 2>&1; then
    uv_interp="$(uv python find 2>/dev/null || true)"
    if [ -z "$uv_interp" ]; then
      uv python install >/dev/null 2>&1 || true
      uv_interp="$(uv python find 2>/dev/null || true)"
    fi
    if [ -n "$uv_interp" ] &&
       "$uv_interp" -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 9) else 1)' 2>/dev/null; then
      candidate=uv
    fi
  fi

  if [ -z "$candidate" ]; then
    if oms_platform_is_windows; then
      echo "error: Python 3.9+ is required; install Python and rerun" >&2
    else
      echo "error: a Python 3.9+ 'python3' command is required (installing uv also satisfies this)" >&2
    fi
    exit 1
  fi

  # A shim we wrote whose interpreter has since moved is repairable, and
  # refusing it would mean no reinstall can ever fix what an install created.
  # Anything else at that path is the user's launcher and stays untouched.
  if [ -e "$shim" ] || [ -L "$shim" ]; then
    if oms_install_python_shim_owned "$shim"; then
      rm -f "$shim"
      echo "replacing stale managed python3 shim: $shim"
    else
      echo "error: refusing to replace existing Python launcher: $shim" >&2
      exit 1
    fi
  fi
  mkdir -p "$HOME/.local/bin"
  case "$candidate" in
    py)
      printf '%s\n' '#!/usr/bin/env bash' '# managed by oh-my-setting' \
        'exec py -3 "$@"' > "$shim"
      ;;
    uv)
      printf '%s\n' '#!/usr/bin/env bash' '# managed by oh-my-setting' \
        'exec "$(uv python find)" "$@"' > "$shim"
      ;;
    *)
      printf '%s\n' '#!/usr/bin/env bash' '# managed by oh-my-setting' \
        'exec python "$@"' > "$shim"
      ;;
  esac
  chmod +x "$shim"
  echo "python3 shim: $shim -> $candidate"
}

install_git_if_missing

if [ "$REF" = "edge" ]; then
  if [ -d "$DEST/.git" ]; then
    git -C "$DEST" fetch --prune origin
    git -C "$DEST" remote set-head origin -a >/dev/null
    remote_head="$(git -C "$DEST" symbolic-ref --quiet --short refs/remotes/origin/HEAD || true)"
    [ -n "$remote_head" ] || { echo "error: cannot resolve origin default branch" >&2; exit 1; }
    edge_branch="${remote_head#origin/}"
    if git -C "$DEST" show-ref --verify --quiet "refs/heads/$edge_branch"; then
      git -C "$DEST" checkout "$edge_branch"
    else
      git -C "$DEST" checkout -b "$edge_branch" --track "$remote_head"
    fi
    git -C "$DEST" pull --ff-only origin "$edge_branch"
  else
    mkdir -p "$(dirname "$DEST")"
    git clone "$REPO_URL" "$DEST"
  fi
else
  if [ ! -d "$DEST/.git" ]; then
    mkdir -p "$(dirname "$DEST")"
    git clone --no-checkout "$REPO_URL" "$DEST"
  fi
  git -C "$DEST" fetch --prune --tags origin
  target="$(git -C "$DEST" rev-parse --verify --quiet "refs/tags/${REF}^{commit}" || true)"
  if [ -z "$target" ]; then
    target="$(git -C "$DEST" rev-parse --verify --quiet "origin/${REF}^{commit}" || true)"
  fi
  if [ -z "$target" ]; then
    target="$(git -C "$DEST" rev-parse --verify --quiet "${REF}^{commit}" || true)"
  fi
  [ -n "$target" ] || { echo "error: cannot resolve install ref: $REF" >&2; exit 1; }
  git -C "$DEST" checkout --detach "$target"
fi

# Continue from the updated checkout so old piped/local installers do not run stale logic.
if [ "${OH_MY_SETTING_REEXECED:-0}" != "1" ] && [ -f "$DEST/install.sh" ]; then
  export OH_MY_SETTING_REEXECED=1
  exec bash "$DEST/install.sh"
fi

# One failure past this point can leave a partial install: tools present but
# targets unlinked, or hooks half-registered. The installer is idempotent and
# doctor relinks from the receipt, so a crash should name that recovery path
# instead of ending with only the failing tool's error.
install_failed() {
  local code="$1"

  [ "$code" -ne 0 ] || return 0
  {
    echo "error: install failed (exit $code)"
    echo "recover: rerun this installer, or run $DEST/scripts/doctor.sh --repair to fix links from the receipt"
  } >&2
}
trap 'install_failed "$?"' EXIT

# shellcheck disable=SC1091
. "$DEST/scripts/lib/platform.sh"
ensure_python3

if [ "$INSTALL_TOOLS" = "1" ]; then
  "$DEST/scripts/install-tools.sh"
  export OH_MY_SETTING_REQUIRE_TOOLS="${OH_MY_SETTING_REQUIRE_TOOLS:-1}"
  load_user_tool_paths
elif [ "$INSTALL_TOOLS" = "0" ]; then
  echo "skipping tool install: OH_MY_SETTING_INSTALL_TOOLS=0"
  export OH_MY_SETTING_REQUIRE_TOOLS="${OH_MY_SETTING_REQUIRE_TOOLS:-0}"
  load_user_tool_paths
else
  echo "error: OH_MY_SETTING_INSTALL_TOOLS must be 0 or 1" >&2
  exit 2
fi

case "$CODEX_PLUGIN" in
  auto)
    if command -v codex >/dev/null 2>&1; then
      CODEX_PLUGIN=1
    else
      CODEX_PLUGIN=0
    fi
    ;;
  0|1) ;;
  *)
    echo "error: OH_MY_SETTING_CODEX_PLUGIN must be 0, 1, or auto" >&2
    exit 2
    ;;
esac
export OH_MY_SETTING_CODEX_PLUGIN="$CODEX_PLUGIN"

"$DEST/scripts/link.sh"

case "$CONNECT_SERVICES" in
  auto|required)
    connect_args=("--$CONNECT_SERVICES")
    if [ -n "$NOTION_DATA_SOURCE_ID" ]; then
      connect_args+=(--data-source-id "$NOTION_DATA_SOURCE_ID")
    fi
    "$DEST/scripts/connect-services.sh" "${connect_args[@]}"
    ;;
  0)
    if [ -n "$NOTION_DATA_SOURCE_ID" ]; then
      "$DEST/scripts/journal.sh" configure \
        --data-source-id "$NOTION_DATA_SOURCE_ID" --no-validate
    fi
    echo "skipping GitHub/Notion connection: OH_MY_SETTING_CONNECT_SERVICES=0"
    ;;
esac

# Claude Code skill-router/turn-guard hooks and usage HUD. Additive
# settings.json merge; Claude-only, non-fatal on failure.
if [ "${OH_MY_SETTING_CLAUDE_HOOKS:-1}" = "1" ]; then
  "$DEST/scripts/install-claude-hooks.sh" ||
    echo "warning: claude settings registration failed (install continues)" >&2
else
  echo "skipping claude hooks/HUD: OH_MY_SETTING_CLAUDE_HOOKS=0"
fi

if [ "$CODEX_PLUGIN" = "1" ]; then
  "$DEST/scripts/install-codex-plugin.sh"
else
  echo "skipping codex plugin registration: OH_MY_SETTING_CODEX_PLUGIN=0"
fi

# Harness-state MCP server: claude and codex register it directly,
# antigravity receives it through the agy plugin. Non-fatal on failure; a
# missing CLI is a note, not an error.
"$DEST/scripts/install-mcp.sh" ||
  echo "warning: MCP server registration failed (install continues)" >&2
"$DEST/scripts/install-agy-plugin.sh" ||
  echo "warning: antigravity plugin import failed (install continues)" >&2

# Antigravity is the one provider whose headless calls are auto-denied without
# standing permissions; codex and claude carry authority per invocation. The
# grant widens what another program may do, so it stays behind an explicit
# flag — a default install only reports what a headless peer would be denied,
# instead of leaving that to be discovered after the first silent council.
if command -v agy >/dev/null 2>&1; then
  if [ "$PEER_PERMISSIONS" = "1" ]; then
    "$DEST/scripts/provider-permissions.sh" --apply --profile consult ||
      echo "warning: peer permission grant failed (install continues)" >&2
  elif ! "$DEST/scripts/provider-permissions.sh" --check; then
    echo "note: peer permissions not granted; rerun with --peer-permissions to allow headless antigravity councils"
  fi
fi

if [ "$GENERATE_SLURM" = "1" ] || { [ "$GENERATE_SLURM" = "auto" ] && command -v sinfo >/dev/null 2>&1; }; then
  "$DEST/scripts/generate-slurm-reference.sh"
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
