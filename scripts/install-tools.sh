#!/usr/bin/env bash
set -euo pipefail

# Install the tool CLIs the harness expects (node via nvm, provider CLIs).

NODE_VERSION="${OH_MY_SETTING_NODE_VERSION:-lts/*}"
NVM_VERSION="${OH_MY_SETTING_NVM_VERSION:-v0.40.3}"
NVM_DIR="${NVM_DIR:-$HOME/.nvm}"
UPGRADE=0
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
# shellcheck source=scripts/lib/platform.sh
. "$ROOT/scripts/lib/platform.sh"

usage() {
  cat <<'EOF'
Usage: install-tools.sh [--upgrade] [-h|--help]

Install missing harness tools: Node (via nvm), uv, the three provider CLIs
(claude, codex, agy), and gh. --upgrade also refreshes existing provider CLIs,
gh, and uv; an existing nvm-managed Node is refreshed to the configured channel.

Nothing here needs root: npm globals go to the nvm prefix and gh is installed
into ~/.local/bin from the official release archive (or brew on macOS).
EOF
}

[ "$#" -le 1 ] || { usage >&2; exit 2; }
case "${1:-}" in
  "") ;;
  --upgrade) UPGRADE=1 ;;
  -h|--help) usage; exit 0 ;;
  *) echo "error: unknown option: $1" >&2; usage >&2; exit 2 ;;
esac

has_cmd() {
  command -v "$1" >/dev/null 2>&1
}

node_major() {
  node -p "process.versions.node.split('.')[0]" 2>/dev/null | grep -E '^[0-9]+$' || echo 0
}

load_nvm() {
  export NVM_DIR
  if [ -s "$NVM_DIR/nvm.sh" ]; then
    # shellcheck disable=SC1091
    . "$NVM_DIR/nvm.sh"
  fi
}

ensure_local_bin_path() {
  local line='export PATH="$HOME/.local/bin:$PATH"'

  export PATH="$HOME/.local/bin:$PATH"
  mkdir -p "$HOME/.local/bin"

  if [ -f "$HOME/.bashrc" ] && grep -Fqx "$line" "$HOME/.bashrc"; then
    return 0
  fi

  printf '\n%s\n' "$line" >> "$HOME/.bashrc"
}

install_nvm() {
  if [ -s "$NVM_DIR/nvm.sh" ]; then
    return 0
  fi

  if oms_platform_is_windows; then
    echo "error: automatic nvm setup is disabled on Windows Git Bash" >&2
    echo "error: install Node.js 20+ for Windows, reopen Git Bash, and rerun" >&2
    exit 1
  fi

  if ! has_cmd curl; then
    echo "error: curl is required to install nvm" >&2
    exit 1
  fi

  echo "installing nvm $NVM_VERSION"
  curl -fsSL "https://raw.githubusercontent.com/nvm-sh/nvm/$NVM_VERSION/install.sh" | bash
}

ensure_node() {
  if has_cmd node && has_cmd npm && [ "$(node_major)" -ge 20 ]; then
    if [ "$UPGRADE" = 1 ] && [ -s "$NVM_DIR/nvm.sh" ]; then
      load_nvm
      nvm install "$NODE_VERSION"
      nvm alias default "$NODE_VERSION"
      nvm use default
    fi
    echo "ok: node $(node --version)"
    return 0
  fi

  install_nvm
  load_nvm

  if ! has_cmd nvm; then
    echo "error: nvm install failed or nvm not loadable" >&2
    exit 1
  fi

  nvm install "$NODE_VERSION"
  nvm alias default "$NODE_VERSION"
  nvm use default
  echo "ok: node $(node --version)"
}

ensure_writable_npm_global() {
  local prefix
  prefix="$(npm config get prefix)"

  if [ -w "$prefix" ]; then
    return 0
  fi

  echo "npm global prefix not writable: $prefix"
  if oms_platform_is_windows; then
    # Windows npm places command shims directly in its prefix (rather than
    # PREFIX/bin). Keep that prefix in the already managed user-local bin so
    # provider CLIs install without Administrator privileges.
    npm config set prefix "$HOME/.local/bin"
    export PATH="$HOME/.local/bin:$PATH"
    echo "using user npm prefix: $HOME/.local/bin"
    return 0
  fi

  echo "switching to nvm-managed Node"

  install_nvm
  load_nvm
  nvm install "$NODE_VERSION"
  nvm alias default "$NODE_VERSION"
  nvm use default
}

install_npm_global() {
  local package="$1"
  local binary="$2"

  if has_cmd "$binary" && [ "$UPGRADE" != 1 ]; then
    echo "ok: $binary already installed"
    return 0
  fi

  echo "installing $package"
  npm install -g "$package"
}

write_npm_shim() {
  local binary="$1"
  local actual
  local target="$HOME/.local/bin/$binary"

  actual="$(command -v "$binary" 2>/dev/null || true)"
  case "$actual" in
    "$NVM_DIR"/*) ;;
    *) return 0 ;;
  esac

  mkdir -p "$HOME/.local/bin"
  cat > "$target" <<EOF
#!/usr/bin/env bash
set -euo pipefail

export NVM_DIR="\${NVM_DIR:-\$HOME/.nvm}"
if [ -s "\$NVM_DIR/nvm.sh" ]; then
  # shellcheck disable=SC1091
  . "\$NVM_DIR/nvm.sh"
  nvm use default >/dev/null 2>&1 || true
fi

prefix="\$(npm prefix -g)"
exec "\$prefix/bin/$binary" "\$@"
EOF
  chmod +x "$target"
  echo "ok: shim $target"
}

install_antigravity() {
  if has_cmd agy && { [ "$UPGRADE" != 1 ] || [ "${OH_MY_SETTING_UPGRADE_ANTIGRAVITY:-1}" = 0 ]; }; then
    echo "ok: agy already installed"
    return 0
  fi

  if ! has_cmd curl; then
    echo "error: curl is required to install Antigravity CLI" >&2
    exit 1
  fi

  echo "installing Antigravity CLI"
  local installer
  installer="$(mktemp)"
  curl -fsSL https://antigravity.google/cli/install.sh -o "$installer"
  bash "$installer"
  rm -f "$installer"

  if ! has_cmd agy; then
    echo "error: Antigravity CLI install completed but agy is not on PATH" >&2
    exit 1
  fi

  echo "ok: agy $(agy --version)"
}

# The GitHub CLI is a harness dependency, not a convenience: github-source.sh
# fetches trusted reusable files through it, ci-status.sh reads the last run
# through it, and PR work goes through it. Installed without sudo on purpose —
# a setup script that needs root is a setup script that gets run as root.
install_gh() {
  if has_cmd gh && [ "$UPGRADE" != 1 ]; then
    echo "ok: gh already installed"
    return 0
  fi

  if [ "$(uname -s)" = "Darwin" ] && has_cmd brew; then
    echo "installing gh via brew"
    if has_cmd gh; then brew upgrade gh || true; else brew install gh; fi
  else
    if ! has_cmd curl || ! has_cmd tar; then
      echo "error: curl and tar are required to install gh" >&2
      exit 1
    fi
    local arch version url tmp
    case "$(uname -m)" in
      x86_64|amd64) arch=amd64 ;;
      aarch64|arm64) arch=arm64 ;;
      *) echo "error: no gh build for $(uname -m); install it manually" >&2; exit 1 ;;
    esac
    local os
    case "$(uname -s)" in
      Linux) os=linux ;;
      Darwin) os=macOS ;;
      *) echo "error: no gh build for $(uname -s); install it manually" >&2; exit 1 ;;
    esac
    # Ask the release API for the current tag; a pinned fallback keeps a rate
    # limited or offline API from turning into a failed install.
    version="$(curl -fsSL https://api.github.com/repos/cli/cli/releases/latest 2>/dev/null |
      sed -n 's/.*"tag_name": *"v\([^"]*\)".*/\1/p' | head -n 1)"
    [ -n "$version" ] || version="${OH_MY_SETTING_GH_VERSION:-2.63.2}"
    url="https://github.com/cli/cli/releases/download/v${version}/gh_${version}_${os}_${arch}.tar.gz"
    echo "installing gh ${version}"
    tmp="$(mktemp -d)"
    if ! curl -fsSL "$url" -o "$tmp/gh.tar.gz"; then
      rm -rf "$tmp"
      echo "error: could not download $url" >&2
      exit 1
    fi
    tar -xzf "$tmp/gh.tar.gz" -C "$tmp"
    # Reuse the one place that owns ~/.local/bin: it also persists the PATH
    # line, so the gh installed here is still found in the next shell.
    ensure_local_bin_path
    if [ -f "$tmp/gh_${version}_${os}_${arch}/bin/gh" ]; then
      install -m 0755 "$tmp/gh_${version}_${os}_${arch}/bin/gh" "$HOME/.local/bin/gh"
    else
      rm -rf "$tmp"
      echo "error: gh archive did not contain the expected binary" >&2
      exit 1
    fi
    rm -rf "$tmp"
  fi

  if ! has_cmd gh; then
    echo "error: gh install completed but gh is not on PATH" >&2
    exit 1
  fi
  # Not reporting authentication here: install.sh runs doctor at the end, and
  # doctor already reads the same local credential and warns — alongside the
  # three provider CLIs, in one place instead of two saying the same thing.
  echo "ok: $(gh --version | head -n 1)"
}

ensure_uv() {
  export PATH="$HOME/.local/bin:$PATH"

  if has_cmd uv; then
    if [ "$UPGRADE" = 1 ]; then
      uv self update
    fi
    echo "ok: uv $(uv --version)"
    return 0
  fi

  if ! has_cmd curl; then
    echo "error: curl is required to install uv" >&2
    exit 1
  fi

  echo "installing uv"
  curl -LsSf https://astral.sh/uv/install.sh | sh
  export PATH="$HOME/.local/bin:$PATH"

  if ! has_cmd uv; then
    echo "error: uv install completed but uv is not on PATH" >&2
    exit 1
  fi

  echo "ok: uv $(uv --version)"
}

ensure_local_bin_path
ensure_node
ensure_writable_npm_global
ensure_uv

install_npm_global "@anthropic-ai/claude-code" "claude"
install_npm_global "@openai/codex" "codex"
install_antigravity
install_gh

write_npm_shim "claude"
write_npm_shim "codex"

echo "tools: ok"
