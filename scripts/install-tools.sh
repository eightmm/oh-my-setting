#!/usr/bin/env bash
set -euo pipefail

# Install the tool CLIs the harness expects (node via nvm, provider CLIs).

NODE_VERSION="${OH_MY_SETTING_NODE_VERSION:-lts/*}"
NVM_VERSION="${OH_MY_SETTING_NVM_VERSION:-v0.40.3}"
NVM_DIR="${NVM_DIR:-$HOME/.nvm}"
UPGRADE=0

usage() {
  cat <<'EOF'
Usage: install-tools.sh [--upgrade] [-h|--help]

Install missing harness tools. --upgrade also refreshes existing provider CLIs
and uv; an existing nvm-managed Node is refreshed to the configured channel.
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

write_npm_shim "claude"
write_npm_shim "codex"

echo "tools: ok"
