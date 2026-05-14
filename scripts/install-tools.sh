#!/usr/bin/env bash
set -euo pipefail

NODE_VERSION="${OH_MY_SETTING_NODE_VERSION:-lts/*}"
NVM_VERSION="${OH_MY_SETTING_NVM_VERSION:-v0.40.3}"
NVM_DIR="${NVM_DIR:-$HOME/.nvm}"
INSTALL_CAVEMAN="${OH_MY_SETTING_INSTALL_CAVEMAN:-1}"

has_cmd() {
  command -v "$1" >/dev/null 2>&1
}

node_major() {
  node -p "process.versions.node.split('.')[0]" 2>/dev/null || echo 0
}

load_nvm() {
  export NVM_DIR
  if [ -s "$NVM_DIR/nvm.sh" ]; then
    # shellcheck disable=SC1091
    . "$NVM_DIR/nvm.sh"
  fi
}

install_nvm() {
  if [ -s "$NVM_DIR/nvm.sh" ]; then
    return 0
  fi

  if ! has_cmd curl; then
    echo "error: curl is required to install nvm" >&2
    exit 1
  fi

  echo "installing nvm $NVM_VERSION"
  curl -fsSL "https://raw.githubusercontent.com/nvm-sh/nvm/$NVM_VERSION/install.sh" | bash
}

ensure_node() {
  if has_cmd node && has_cmd npm && [ "$(node_major)" -ge 18 ]; then
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

  if has_cmd "$binary"; then
    echo "ok: $binary already installed"
    return 0
  fi

  echo "installing $package"
  npm install -g "$package"
}

ensure_uv() {
  export PATH="$HOME/.local/bin:$PATH"

  if has_cmd uv; then
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

ensure_node
ensure_writable_npm_global
ensure_uv

install_npm_global "@anthropic-ai/claude-code" "claude"
install_npm_global "@openai/codex" "codex"
install_npm_global "@google/gemini-cli" "gemini"
install_npm_global "@earendil-works/pi-coding-agent" "pi"

if [ "$INSTALL_CAVEMAN" != "0" ]; then
  echo "installing caveman for detected agents"
  curl -fsSL https://raw.githubusercontent.com/JuliusBrussee/caveman/main/install.sh | bash
fi

echo "tools: ok"
