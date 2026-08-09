#!/usr/bin/env bash
set -euo pipefail

# Install the exact tool CLI releases in tools.lock.json. Downloads are never
# executed or extracted before their repository-pinned digest is verified.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
TOOL_LOCK="${OH_MY_SETTING_TOOL_LOCK:-$ROOT/tools.lock.json}"
TOOL_LOCK_HELPER="$ROOT/scripts/lib/tool-lock.py"
NVM_DIR="${NVM_DIR:-$HOME/.nvm}"
UPGRADE=0
# shellcheck source=scripts/lib/platform.sh
. "$ROOT/scripts/lib/platform.sh"

[ -x "$TOOL_LOCK_HELPER" ] || { echo "error: missing tool lock helper: $TOOL_LOCK_HELPER" >&2; exit 1; }
python3 "$TOOL_LOCK_HELPER" --lock "$TOOL_LOCK" validate >/dev/null || exit $?
tool_lock_get() {
  # Native Windows Python writes CRLF; every scalar that crosses back into Bash
  # is normalized here before it can become a path, version, or digest.
  python3 "$TOOL_LOCK_HELPER" --lock "$TOOL_LOCK" get "$1" | tr -d '\r'
}
NODE_VERSION="$(tool_lock_get node.version)"
NVM_VERSION="$(tool_lock_get nvm.version)"

usage() {
  cat <<'EOF'
Usage: install-tools.sh [--upgrade] [-h|--help]

Install missing harness tools at the exact versions and checksums recorded in
tools.lock.json: Node (via nvm), uv, the three provider CLIs (claude, codex,
agy), gh, and ntn. --upgrade refreshes them to that same lock.

Nothing here needs root: npm globals use the active writable prefix or
~/.local, and direct release payloads such as gh are installed in ~/.local/bin.
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

sha256_file() {
  if has_cmd sha256sum; then sha256sum "$1" | awk '{print $1}'
  elif has_cmd shasum; then shasum -a 256 "$1" | awk '{print $1}'
  elif has_cmd openssl; then openssl dgst -sha256 "$1" | awk '{print $NF}'
  else echo "error: sha256sum, shasum, or openssl is required" >&2; return 1
  fi
}

sha512_file() {
  if has_cmd sha512sum; then sha512sum "$1" | awk '{print $1}'
  elif has_cmd shasum; then shasum -a 512 "$1" | awk '{print $1}'
  elif has_cmd openssl; then openssl dgst -sha512 "$1" | awk '{print $NF}'
  else echo "error: sha512sum, shasum, or openssl is required" >&2; return 1
  fi
}

verify_digest() {  # ALGORITHM EXPECTED FILE LABEL
  local algorithm="$1" expected="$2" file="$3" label="$4" actual=""
  case "$algorithm" in
    sha256) actual="$(sha256_file "$file")" || return 1 ;;
    sha512) actual="$(sha512_file "$file")" || return 1 ;;
    *) echo "error: unsupported checksum algorithm: $algorithm" >&2; return 1 ;;
  esac
  if [ "$actual" != "$expected" ]; then
    echo "error: checksum mismatch for $label" >&2
    echo "error: expected $expected, got ${actual:-unavailable}" >&2
    return 1
  fi
}

download_locked() {  # URL DEST
  has_cmd curl || { echo "error: curl is required to install locked tools" >&2; return 1; }
  curl --proto '=https' --tlsv1.2 -fsSL "$1" -o "$2"
}

locked_platform() {
  local os arch
  if oms_platform_is_windows; then
    os=windows
  else
    case "$(uname -s)" in
      Linux) os=linux ;;
      Darwin) os=darwin ;;
      *) echo "error: unsupported tool platform: $(uname -s)" >&2; return 1 ;;
    esac
  fi
  case "$(uname -m)" in
    x86_64|amd64) arch=amd64 ;;
    aarch64|arm64) arch=arm64 ;;
    *) echo "error: unsupported tool architecture: $(uname -m)" >&2; return 1 ;;
  esac
  printf '%s-%s\n' "$os" "$arch"
}

node_exact_version() {
  node --version 2>/dev/null | tr -d '\r' | sed 's/^v//'
}

load_nvm() {
  local expected
  export NVM_DIR
  if [ -s "$NVM_DIR/nvm.sh" ]; then
    expected="$(tool_lock_get nvm.script_sha256)"
    verify_digest sha256 "$expected" "$NVM_DIR/nvm.sh" "existing nvm.sh" || {
      echo "error: refusing to source an nvm.sh that does not match tools.lock.json" >&2
      return 1
    }
    # shellcheck disable=SC1091
    . "$NVM_DIR/nvm.sh"
  fi
}

extract_locked() {  # ARCHIVE SOURCE DESTINATION
  mkdir -p "$3"
  python3 "$TOOL_LOCK_HELPER" --lock "$TOOL_LOCK" extract \
    --archive "$1" --source "$2" --dest "$3"
}

command_has_version() {  # COMMAND EXPECTED
  local output
  output="$("$1" --version 2>/dev/null | tr -d '\r' || true)"
  printf '%s' "$output" | python3 -c '
import re, sys
expected = re.escape(sys.argv[1].lstrip("v"))
raise SystemExit(0 if re.search(r"(?<![0-9A-Za-z.+-])v?%s(?![0-9A-Za-z.+-])" % expected, sys.stdin.read()) else 1)
' "$2"
}

ensure_local_bin_path() {
  local line='export PATH="$HOME/.local/bin:$PATH"'
  local rc bash_login

  export PATH="$HOME/.local/bin:$PATH"
  mkdir -p "$HOME/.local/bin"

  # Bash reads only the first existing login file. Never create .bash_profile
  # over an existing .bash_login/.profile, which would hide the user's setup.
  if [ -e "$HOME/.bash_profile" ]; then
    bash_login="$HOME/.bash_profile"
  elif [ -e "$HOME/.bash_login" ]; then
    bash_login="$HOME/.bash_login"
  else
    bash_login="$HOME/.profile"
  fi

  # Bash interactive/login shells and zsh interactive/login shells read
  # different entry points. Persist one idempotent line in each actual path.
  for rc in "$HOME/.bashrc" "$bash_login" "$HOME/.zshrc" "$HOME/.zprofile"; do
    if [ -f "$rc" ] && grep -Fqx "$line" "$rc"; then
      continue
    fi
    printf '\n%s\n' "$line" >> "$rc"
  done
}

install_nvm() {
  local url expected script_expected tmp extracted stage marker
  script_expected="$(tool_lock_get nvm.script_sha256)"
  if [ -s "$NVM_DIR/nvm.sh" ]; then
    verify_digest sha256 "$script_expected" "$NVM_DIR/nvm.sh" "existing nvm.sh" || {
      echo "error: refusing to source an nvm.sh that does not match tools.lock.json" >&2
      exit 1
    }
    return 0
  fi

  if oms_platform_is_windows; then
    echo "error: automatic nvm setup is disabled on Windows Git Bash" >&2
    echo "error: install Node.js v$NODE_VERSION for Windows, reopen Git Bash, and rerun" >&2
    exit 1
  fi

  echo "installing nvm $NVM_VERSION"
  url="$(tool_lock_get nvm.url)"
  expected="$(tool_lock_get nvm.sha256)"
  stage="$NVM_DIR.oh-my-setting-stage"
  marker="$stage/.oh-my-setting-complete"

  # A previous process may have stopped after staging but before the same-FS
  # rename. A complete verified stage is promoted; an incomplete stage is
  # owned by this installer and safely rebuilt.
  if [ -e "$stage" ]; then
    if [ "$(sed -n '1p' "$marker" 2>/dev/null || true)" = "$expected" ] &&
       [ -f "$stage/nvm.sh" ] &&
       verify_digest sha256 "$script_expected" "$stage/nvm.sh" "staged nvm.sh"; then
      [ ! -e "$NVM_DIR" ] || {
        echo "error: both NVM_DIR and its recovery stage exist: $NVM_DIR" >&2
        exit 1
      }
      mv "$stage" "$NVM_DIR"
      return 0
    fi
    rm -rf "$stage"
  fi

  if [ -e "$NVM_DIR" ]; then
    if [ -d "$NVM_DIR" ] && [ -z "$(ls -A "$NVM_DIR" 2>/dev/null)" ]; then
      rmdir "$NVM_DIR"
    else
      echo "error: refusing to replace non-empty NVM_DIR without nvm.sh: $NVM_DIR" >&2
      exit 1
    fi
  fi

  tmp="$(mktemp -d "${TMPDIR:-/tmp}/oms-nvm.XXXXXX")"
  if ! download_locked "$url" "$tmp/nvm.tar.gz" ||
     ! verify_digest sha256 "$expected" "$tmp/nvm.tar.gz" "nvm $NVM_VERSION"; then
    rm -rf "$tmp"
    exit 1
  fi
  if ! extract_locked tar.gz "$tmp/nvm.tar.gz" "$tmp/extracted"; then
    rm -rf "$tmp"
    exit 1
  fi
  extracted="$tmp/extracted/nvm-${NVM_VERSION#v}"
  [ -f "$extracted/nvm.sh" ] || {
    rm -rf "$tmp"
    echo "error: verified nvm archive has no nvm.sh" >&2
    exit 1
  }
  if ! verify_digest sha256 "$script_expected" "$extracted/nvm.sh" "nvm.sh $NVM_VERSION"; then
    rm -rf "$tmp"
    exit 1
  fi
  mkdir -p "$(dirname "$NVM_DIR")"
  mv "$extracted" "$stage"
  printf '%s\n' "$expected" > "$marker"
  mv "$stage" "$NVM_DIR"
  rm -rf "$tmp"
}

install_locked_node() {
  local platform url expected archive tmp extracted target installed stage marker

  if oms_platform_is_windows; then
    echo "error: automatic Node setup is disabled on Windows Git Bash" >&2
    echo "error: install native Node.js v$NODE_VERSION for Windows, reopen Git Bash, and rerun" >&2
    exit 1
  fi
  platform="$(locked_platform)"
  url="$(tool_lock_get "node.platforms.$platform.url")"
  expected="$(tool_lock_get "node.platforms.$platform.sha256")"
  archive="$(tool_lock_get "node.platforms.$platform.archive")"
  [ "$archive" = tar.gz ] || {
    echo "error: unsupported Node archive: $archive" >&2
    exit 1
  }
  target="$NVM_DIR/versions/node/v$NODE_VERSION"
  if [ -x "$target/bin/node" ]; then
    installed="$("$target/bin/node" --version 2>/dev/null | tr -d '\r' | sed 's/^v//')"
    [ "$installed" = "$NODE_VERSION" ] || {
      echo "error: refusing to replace invalid existing Node directory: $target" >&2
      exit 1
    }
    return 0
  fi
  [ ! -e "$target" ] || {
    echo "error: refusing to replace existing Node directory without a valid binary: $target" >&2
    exit 1
  }

  stage="$target.oh-my-setting-stage"
  marker="$stage/.oh-my-setting-complete"
  if [ -e "$stage" ]; then
    if [ "$(sed -n '1p' "$marker" 2>/dev/null || true)" = "$expected" ] &&
       [ -x "$stage/bin/node" ] &&
       [ "$("$stage/bin/node" --version 2>/dev/null | tr -d '\r')" = "v$NODE_VERSION" ]; then
      mkdir -p "$(dirname "$target")"
      mv "$stage" "$target"
      return 0
    fi
    rm -rf "$stage"
  fi

  echo "installing Node $NODE_VERSION ($platform)"
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/oms-node.XXXXXX")"
  if ! download_locked "$url" "$tmp/node.tar.gz" ||
     ! verify_digest sha256 "$expected" "$tmp/node.tar.gz" "Node $NODE_VERSION ($platform)"; then
    rm -rf "$tmp"
    exit 1
  fi
  if ! extract_locked tar.gz "$tmp/node.tar.gz" "$tmp/extracted"; then
    rm -rf "$tmp"
    exit 1
  fi
  extracted="$tmp/extracted/$(basename "$url" .tar.gz)"
  if [ ! -x "$extracted/bin/node" ] ||
     [ "$("$extracted/bin/node" --version 2>/dev/null | tr -d '\r')" != "v$NODE_VERSION" ]; then
    rm -rf "$tmp"
    echo "error: verified Node archive does not contain Node v$NODE_VERSION" >&2
    exit 1
  fi
  mkdir -p "$(dirname "$target")"
  mv "$extracted" "$stage"
  printf '%s\n' "$expected" > "$marker"
  mv "$stage" "$target"
  rm -rf "$tmp"
}

activate_locked_node() {
  load_nvm
  if ! has_cmd nvm; then
    echo "error: nvm is not loadable after setup" >&2
    exit 1
  fi
  nvm alias default "$NODE_VERSION"
  nvm use default
}

ensure_node() {
  if has_cmd node && has_cmd npm && [ "$(node_exact_version)" = "$NODE_VERSION" ]; then
    echo "ok: node $(node --version)"
    return 0
  fi

  if oms_platform_is_windows; then
    echo "error: Windows Git Bash requires native Node.js v$NODE_VERSION and npm" >&2
    echo "error: install the exact locked release, reopen Git Bash, and rerun" >&2
    exit 1
  fi

  install_nvm
  install_locked_node
  activate_locked_node
  echo "ok: node $(node --version)"
}

normalize_npm_path() {  # PATH_FROM_NATIVE_NPM
  local value="${1//$'\r'/}"
  if oms_platform_is_windows; then
    has_cmd cygpath || {
      echo "error: cygpath is required with native npm in Windows Git Bash" >&2
      return 1
    }
    value="$(cygpath -u "$value" | tr -d '\r')"
  fi
  if [ -d "$value" ]; then
    (cd "$value" && pwd -P)
  else
    printf '%s\n' "$value"
  fi
}

ensure_writable_npm_global() {
  local prefix user_prefix
  prefix="$(normalize_npm_path "$(npm config get prefix | tr -d '\r')")"

  if [ -w "$prefix" ]; then
    return 0
  fi

  echo "npm global prefix not writable: $prefix"
  if oms_platform_is_windows; then
    # Windows npm places command shims directly in its prefix (rather than
    # PREFIX/bin). Keep that prefix in the already managed user-local bin so
    # provider CLIs install without Administrator privileges.
    user_prefix="$HOME/.local/bin"
  else
    # An exact active Node does not imply that an unrelated ~/.nvm is trusted.
    # Use npm's supported user prefix instead of sourcing or replacing it.
    user_prefix="$HOME/.local"
  fi
  mkdir -p "$user_prefix"
  user_prefix="$(normalize_npm_path "$user_prefix")"
  npm config set prefix "$user_prefix"
  export PATH="$HOME/.local/bin:$PATH"
  prefix="$(normalize_npm_path "$(npm config get prefix | tr -d '\r')")"
  [ "$prefix" = "$user_prefix" ] || {
    echo "error: npm did not activate the requested user prefix: $user_prefix" >&2
    exit 1
  }
  echo "using user npm prefix: $user_prefix"
}

installed_npm_version() {  # PACKAGE
  local listing
  listing="$(npm list -g --depth=0 --json "$1" 2>/dev/null || true)"
  printf '%s' "$listing" | python3 -c '
import json, sys
try:
    row = json.load(sys.stdin)
    version = row.get("dependencies", {}).get(sys.argv[1], {}).get("version", "")
except (AttributeError, json.JSONDecodeError):
    version = ""
if isinstance(version, str):
    print(version)
' "$1" | tr -d '\r'
}

npm_global_prefix() {
  local value
  value="$(npm prefix -g 2>/dev/null | tr -d '\r')" || return 1
  normalize_npm_path "$value"
}

npm_global_root() {
  local value
  value="$(npm root -g 2>/dev/null | tr -d '\r')" || return 1
  normalize_npm_path "$value"
}

npm_global_bin_dir() {
  local prefix
  prefix="$(npm_global_prefix)"
  [ -n "$prefix" ] || return 1
  if oms_platform_is_windows; then
    printf '%s\n' "$prefix"
  else
    printf '%s/bin\n' "$prefix"
  fi
}

managed_npm_binary() {  # BINARY
  local directory candidate
  directory="$(npm_global_bin_dir)" || return 1
  for candidate in "$directory/$1" "$directory/$1.exe" "$directory/$1.cmd"; do
    if [ -f "$candidate" ]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  return 1
}

npm_native_platform() {  # LOCK NAME
  local name="$1" platform libc
  platform="$(locked_platform)"
  if [ "$name" = claude ]; then
    case "$platform" in
      linux-*)
        libc="$(node -p 'process.report && process.report.getReport().header.glibcVersionRuntime ? "glibc" : "musl"' 2>/dev/null | tr -d '\r')"
        [ "$libc" != musl ] || platform="$platform-musl"
        ;;
    esac
  fi
  printf '%s\n' "$platform"
}

pack_locked_npm() {  # SPEC INTEGRITY EXPECTED_NAME EXPECTED_VERSION TMP STEM
  local spec="$1" locked_integrity="$2" expected_name="$3" expected_version="$4"
  local tmp="$5" stem="$6" package_file
  if ! npm pack "$spec" --ignore-scripts --json --pack-destination "$tmp" \
      > "$tmp/$stem.json"; then
    echo "error: could not download npm package $spec" >&2
    return 1
  fi
  package_file="$(python3 - "$tmp" "$tmp/$stem.json" "$locked_integrity" <<'PY'
import json
import sys
from pathlib import Path

root = Path(sys.argv[1]).resolve()
try:
    rows = json.loads(Path(sys.argv[2]).read_text(encoding="utf-8"))
    if not isinstance(rows, list) or len(rows) != 1 or not isinstance(rows[0], dict):
        raise ValueError
    filename = rows[0].get("filename")
    if not isinstance(filename, str) or Path(filename).name != filename:
        raise ValueError
    if rows[0].get("integrity") != sys.argv[3]:
        raise ValueError
    candidate = (root / filename).resolve(strict=True)
    candidate.relative_to(root)
    if not candidate.is_file():
        raise ValueError
except (OSError, ValueError, json.JSONDecodeError):
    raise SystemExit(1)
print(candidate)
PY
)" || {
    echo "error: npm package metadata does not match tools.lock.json for $spec" >&2
    return 1
  }
  package_file="${package_file//$'\r'/}"
  if ! python3 "$TOOL_LOCK_HELPER" --lock "$TOOL_LOCK" verify-sri \
      --integrity "$locked_integrity" --file "$package_file" >/dev/null; then
    echo "error: npm package bytes do not match tools.lock.json for $spec" >&2
    return 1
  fi
  if ! python3 "$TOOL_LOCK_HELPER" --lock "$TOOL_LOCK" verify-npm-package \
      --file "$package_file" --name "$expected_name" \
      --version "$expected_version" >/dev/null; then
    echo "error: npm package identity does not match tools.lock.json for $spec" >&2
    return 1
  fi
  printf '%s\n' "$package_file"
}

npm_package_is_current() {  # NAME
  local name="$1" package binary version current root package_root managed
  local native_platform native_alias native_package native_version native_path
  package="$(tool_lock_get "npm.$name.package")"
  binary="$(tool_lock_get "npm.$name.binary")"
  version="$(tool_lock_get "npm.$name.version")"
  current="$(installed_npm_version "$package")"
  [ "$current" = "$version" ] || return 1
  root="$(npm_global_root)"
  [ -n "$root" ] || return 1
  package_root="$root/$package"
  python3 "$TOOL_LOCK_HELPER" --lock "$TOOL_LOCK" verify-installed-npm \
    --path "$package_root" --name "$package" --version "$version" >/dev/null || return 1
  case "$name" in
    claude|codex)
      native_platform="$(npm_native_platform "$name")"
      native_alias="$(tool_lock_get "npm.$name.native.$native_platform.alias")"
      native_package="$(tool_lock_get "npm.$name.native.$native_platform.package")"
      native_version="$(tool_lock_get "npm.$name.native.$native_platform.version")"
      native_path="$package_root/node_modules/$native_alias"
      python3 "$TOOL_LOCK_HELPER" --lock "$TOOL_LOCK" verify-installed-npm \
        --path "$native_path" --name "$native_package" \
        --version "$native_version" >/dev/null || return 1
      ;;
  esac
  managed="$(managed_npm_binary "$binary")" || return 1
  command_has_version "$managed" "$version"
}

npm_transaction_write() {  # MARKER PHASE HAD_PACKAGE BINARY
  local marker="$1" phase="$2" had_package="$3" binary="$4" staged
  staged="$marker.stage.$$"
  {
    printf 'schema=1\n'
    printf 'phase=%s\n' "$phase"
    printf 'had_package=%s\n' "$had_package"
    printf 'binary=%s\n' "$binary"
  } > "$staged"
  mv "$staged" "$marker"
}

npm_transaction_restore() {  # PACKAGE_ROOT GLOBAL_BIN BINARY
  local package_root="$1" global_bin="$2" binary="$3"
  local marker="$1.oh-my-setting-transaction"
  local package_backup="$1.oh-my-setting-backup"
  local bin_backup="$2/.$3.oh-my-setting-backup"
  local phase had_package candidate saved

  [ -f "$marker" ] || return 0
  [ "$(sed -n '1p' "$marker")" = schema=1 ] || {
    echo "error: invalid npm transaction marker requires manual review: $marker" >&2
    return 1
  }
  phase="$(sed -n 's/^phase=//p' "$marker")"
  had_package="$(sed -n 's/^had_package=//p' "$marker")"
  [ "$(sed -n 's/^binary=//p' "$marker")" = "$binary" ] || return 1
  case "$phase:$had_package" in
    prepared:0|prepared:1|backed_up:0|backed_up:1) ;;
    *) echo "error: invalid npm transaction state: $marker" >&2; return 1 ;;
  esac

  if [ "$phase" = backed_up ]; then
    rm -rf "$package_root"
    for candidate in "$global_bin/$binary" "$global_bin/$binary.exe" \
        "$global_bin/$binary.cmd" "$global_bin/$binary.ps1"; do
      [ ! -e "$candidate" ] && [ ! -L "$candidate" ] || rm -f "$candidate"
    done
  fi

  if [ -e "$package_backup" ] || [ -L "$package_backup" ]; then
    [ ! -e "$package_root" ] && [ ! -L "$package_root" ] || {
      echo "error: npm package and recovery backup both exist: $package_root" >&2
      return 1
    }
    mv "$package_backup" "$package_root"
  elif [ "$phase" = backed_up ] && [ "$had_package" = 1 ]; then
    echo "error: npm package recovery backup is missing: $package_backup" >&2
    return 1
  fi

  if [ -d "$bin_backup" ]; then
    for saved in "$bin_backup"/*; do
      [ -e "$saved" ] || [ -L "$saved" ] || continue
      candidate="$global_bin/$(basename "$saved")"
      [ ! -e "$candidate" ] && [ ! -L "$candidate" ] || {
        echo "error: npm binary and recovery backup both exist: $candidate" >&2
        return 1
      }
      mv "$saved" "$candidate"
    done
    rmdir "$bin_backup"
  fi
  rm -f "$marker"
}

npm_transaction_commit() {  # PACKAGE_ROOT GLOBAL_BIN BINARY
  local package_root="$1" global_bin="$2" binary="$3"
  rm -rf "$package_root.oh-my-setting-backup"
  rm -rf "$global_bin/.$binary.oh-my-setting-backup"
  rm -f "$package_root.oh-my-setting-transaction"
}

npm_transaction_recover() {  # NAME PACKAGE_ROOT GLOBAL_BIN BINARY
  local name="$1" package_root="$2" global_bin="$3" binary="$4"
  local marker="$2.oh-my-setting-transaction" phase
  [ -f "$marker" ] || return 0
  phase="$(sed -n 's/^phase=//p' "$marker")"
  if [ "$phase" = backed_up ] && npm_package_is_current "$name"; then
    npm_transaction_commit "$package_root" "$global_bin" "$binary"
  else
    npm_transaction_restore "$package_root" "$global_bin" "$binary"
  fi
}

npm_transaction_begin() {  # PACKAGE_ROOT GLOBAL_BIN BINARY
  local package_root="$1" global_bin="$2" binary="$3"
  local marker="$1.oh-my-setting-transaction"
  local package_backup="$1.oh-my-setting-backup"
  local bin_backup="$2/.$3.oh-my-setting-backup"
  local had_package=0 candidate

  [ ! -e "$marker" ] && [ ! -e "$package_backup" ] && [ ! -e "$bin_backup" ] || {
    echo "error: stale npm transaction state requires recovery before install" >&2
    return 1
  }
  mkdir -p "$(dirname "$package_root")" "$global_bin"
  if [ -e "$package_root" ] || [ -L "$package_root" ]; then had_package=1; fi
  npm_transaction_write "$marker" prepared "$had_package" "$binary" || return 1

  [ "$had_package" = 0 ] || mv "$package_root" "$package_backup"
  mkdir "$bin_backup"
  for candidate in "$global_bin/$binary" "$global_bin/$binary.exe" \
      "$global_bin/$binary.cmd" "$global_bin/$binary.ps1"; do
    [ ! -e "$candidate" ] && [ ! -L "$candidate" ] ||
      mv "$candidate" "$bin_backup/$(basename "$candidate")"
  done
  npm_transaction_write "$marker" backed_up "$had_package" "$binary" || {
    npm_transaction_restore "$package_root" "$global_bin" "$binary"
    return 1
  }
}

install_npm_global() {
  local name="$1"
  local package binary version locked_integrity spec current tmp package_file
  local root package_root managed native_platform native_alias native_package global_bin
  local native_version native_integrity native_spec native_file

  package="$(tool_lock_get "npm.$name.package")"
  binary="$(tool_lock_get "npm.$name.binary")"
  version="$(tool_lock_get "npm.$name.version")"
  locked_integrity="$(tool_lock_get "npm.$name.integrity")"
  spec="$package@$version"

  root="$(npm_global_root)"
  global_bin="$(npm_global_bin_dir)"
  package_root="$root/$package"
  npm_transaction_recover "$name" "$package_root" "$global_bin" "$binary" || return 1

  if npm_package_is_current "$name"; then
    export PATH="$global_bin:$PATH"
    hash -r
    echo "ok: $binary $version"
    return 0
  fi

  echo "installing $spec"
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/oms-npm.XXXXXX")"
  package_file="$(pack_locked_npm "$spec" "$locked_integrity" \
    "$package" "$version" "$tmp" main)" || {
    rm -rf "$tmp"
    return 1
  }
  package_file="${package_file//$'\r'/}"

  native_file=""
  case "$name" in
    claude|codex)
      native_platform="$(npm_native_platform "$name")"
      native_alias="$(tool_lock_get "npm.$name.native.$native_platform.alias")"
      native_package="$(tool_lock_get "npm.$name.native.$native_platform.package")"
      native_version="$(tool_lock_get "npm.$name.native.$native_platform.version")"
      native_integrity="$(tool_lock_get "npm.$name.native.$native_platform.integrity")"
      native_spec="$native_package@$native_version"
      native_file="$(pack_locked_npm "$native_spec" "$native_integrity" \
        "$native_package" "$native_version" "$tmp" native)" || {
        rm -rf "$tmp"
        return 1
      }
      native_file="${native_file//$'\r'/}"
      ;;
  esac

  # All registry downloads are complete and SRI/identity verified before npm
  # mutates the global tree. A fresh cache prevents npm from satisfying an
  # optional dependency from unrelated, previously cached registry bytes;
  # offline mode makes every other unexpected dependency a hard failure.
  mkdir -p "$tmp/install-cache"
  npm_transaction_begin "$package_root" "$global_bin" "$binary" || {
    rm -rf "$tmp"
    return 1
  }
  if ! npm install -g --cache "$tmp/install-cache" --offline \
      --ignore-scripts --omit=optional \
      --no-audit --no-fund "$package_file"; then
    npm_transaction_restore "$package_root" "$global_bin" "$binary"
    rm -rf "$tmp"
    return 1
  fi

  if ! python3 "$TOOL_LOCK_HELPER" --lock "$TOOL_LOCK" verify-installed-npm \
      --path "$package_root" --name "$package" --version "$version" >/dev/null; then
    npm_transaction_restore "$package_root" "$global_bin" "$binary"
    rm -rf "$tmp"
    echo "error: npm installed an unexpected package for $spec" >&2
    return 1
  fi

  if [ -n "$native_file" ]; then
    if ! python3 "$TOOL_LOCK_HELPER" --lock "$TOOL_LOCK" install-npm-payload \
        --file "$native_file" --dest "$package_root/node_modules/$native_alias" \
        --root "$package_root" \
        --name "$native_package" --version "$native_version" >/dev/null; then
      npm_transaction_restore "$package_root" "$global_bin" "$binary"
      rm -rf "$tmp"
      echo "error: could not install the verified native payload for $spec" >&2
      return 1
    fi
  fi
  case "$name" in
    claude)
      if ! node "$package_root/install.cjs"; then
        npm_transaction_restore "$package_root" "$global_bin" "$binary"
        rm -rf "$tmp"
        return 1
      fi
      ;;
    ntn)
      if ! node "$package_root/install.cjs" install; then
        npm_transaction_restore "$package_root" "$global_bin" "$binary"
        rm -rf "$tmp"
        return 1
      fi
      ;;
  esac

  current="$(installed_npm_version "$package")"
  managed="$(managed_npm_binary "$binary")" || managed=""
  if [ "$current" != "$version" ] || [ -z "$managed" ] ||
     ! npm_package_is_current "$name"; then
    npm_transaction_restore "$package_root" "$global_bin" "$binary"
    rm -rf "$tmp"
    echo "error: npm installed $package ${current:-unknown}, expected managed $version" >&2
    return 1
  fi
  npm_transaction_commit "$package_root" "$global_bin" "$binary"
  rm -rf "$tmp"
  export PATH="$global_bin:$PATH"
  hash -r
}

npm_shim_owned() {  # FILE
  [ -f "$1" ] && [ ! -L "$1" ] || return 1
  [ "$(sed -n '1p' "$1")" = '#!/usr/bin/env bash' ] || return 1
  [ "$(sed -n '4p' "$1")" = '# managed by oh-my-setting; rewritten on every tool update' ]
}

preflight_npm_shim() {  # BINARY
  local binary="$1" target="$HOME/.local/bin/$1" global_bin
  global_bin="$(npm_global_bin_dir)" || return 1
  [ "$global_bin" != "$HOME/.local/bin" ] || return 0
  [ ! -e "$target" ] && [ ! -L "$target" ] && return 0
  npm_shim_owned "$target" && return 0
  echo "error: $target shadows the locked npm-managed $binary command" >&2
  echo "error: move the user-owned shadow and rerun; it was not overwritten" >&2
  return 1
}

write_npm_shim() {
  local binary="$1"
  local actual
  local node_bin
  local target="$HOME/.local/bin/$binary"

  actual="$(managed_npm_binary "$binary")" || {
    echo "error: no npm-managed binary found for $binary" >&2
    return 1
  }
  case "$(dirname "$actual")" in
    "$HOME/.local/bin") return 0 ;;
  esac
  if [ -e "$target" ] || [ -L "$target" ]; then
    npm_shim_owned "$target" || {
      echo "error: refusing to overwrite user-owned PATH shadow: $target" >&2
      return 1
    }
  fi

  mkdir -p "$HOME/.local/bin"
  node_bin="$(dirname "$(command -v node)")"
  {
    printf '#!/usr/bin/env bash\n'
    printf 'set -euo pipefail\n\n'
    printf '# managed by oh-my-setting; rewritten on every tool update\n'
    printf 'export PATH=%q:"$PATH"\n' "$node_bin"
    printf 'exec %q "$@"\n' "$actual"
  } > "$target"
  chmod +x "$target"
  echo "ok: shim $target"
}

verify_resolved_npm_binary() {  # NAME
  local name="$1" binary version resolved target global_bin
  binary="$(tool_lock_get "npm.$name.binary")"
  version="$(tool_lock_get "npm.$name.version")"
  global_bin="$(npm_global_bin_dir)"
  export PATH="$HOME/.local/bin:$global_bin:$PATH"
  hash -r
  resolved="$(command -v "$binary" 2>/dev/null || true)"
  resolved="${resolved//$'\r'/}"
  target="$HOME/.local/bin/$binary"
  case "$resolved" in
    "$target"|"$global_bin/$binary"|"$global_bin/$binary.exe"|"$global_bin/$binary.cmd") ;;
    *) echo "error: resolved $binary is not the locked npm-managed command: ${resolved:-missing}" >&2; return 1 ;;
  esac
  npm_package_is_current "$name" && command_has_version "$resolved" "$version"
}

direct_owner_write() {  # TARGET
  local target="$1" owner="$1.oh-my-setting-managed" staged digest
  digest="$(sha256_file "$target")" || return 1
  staged="$owner.stage.$$"
  printf 'sha256=%s\n' "$digest" > "$staged"
  mv "$staged" "$owner"
}

direct_target_claimed() {  # TARGET [OWNER_SOURCE]
  local target="$1" owner_source="${2:-$1}" owner byte_count
  owner="$owner_source.oh-my-setting-managed"
  [ -f "$target" ] && [ ! -L "$target" ] || return 1
  [ -f "$owner" ] && [ ! -L "$owner" ] || return 1
  byte_count="$(wc -c < "$owner" | tr -d '[:space:]')"
  [ "$byte_count" = 72 ] || return 1
  grep -Eq '^sha256=[0-9a-f]{64}$' "$owner"
}

direct_target_owned() {  # TARGET [OWNER_SOURCE]
  local target="$1" owner_source="${2:-$1}" digest
  direct_target_claimed "$target" "$owner_source" || return 1
  digest="$(sha256_file "$target")" || return 1
  [ "$(sed -n '1p' "$owner_source.oh-my-setting-managed")" = "sha256=$digest" ]
}

direct_locked_command_is_current() {  # COMMAND MANAGED_TARGET VERSION
  local command="$1" target="$2" version="$3" owner
  owner="$target.oh-my-setting-managed"
  if [ -e "$owner" ] || [ -L "$owner" ]; then
    direct_target_owned "$target" && command_has_version "$target" "$version"
  else
    has_cmd "$command" && command_has_version "$command" "$version"
  fi
}

recover_direct_install() {  # TARGET ALGORITHM EXPECTED VERSION
  local target="$1" algorithm="$2" expected="$3" version="$4"
  local stage="$1.oh-my-setting-stage" backup="$1.oh-my-setting-backup"

  if [ -e "$backup" ] || [ -L "$backup" ]; then
    if [ -f "$target" ] && [ ! -L "$target" ] &&
       verify_digest "$algorithm" "$expected" "$target" "recovered tool target" 2>/dev/null &&
       command_has_version "$target" "$version" &&
       { direct_target_owned "$target" || direct_target_claimed "$backup" "$target"; }; then
      direct_owner_write "$target" || return 1
      rm -f "$backup" "$stage"
    elif direct_target_claimed "$backup" "$target"; then
      [ ! -e "$target" ] && [ ! -L "$target" ] || rm -f "$target"
      mv "$backup" "$target"
      rm -f "$stage"
    else
      echo "error: unowned direct-tool recovery backup requires manual review: $backup" >&2
      return 1
    fi
  fi

  # A process can stop after the verified rename but before the ownership
  # sidecar. Exact locked bytes are safe to adopt without replacing the file.
  if [ -f "$target" ] && [ ! -L "$target" ] &&
     ! direct_target_owned "$target" &&
     verify_digest "$algorithm" "$expected" "$target" "recovered tool target" 2>/dev/null &&
     command_has_version "$target" "$version"; then
    direct_owner_write "$target" || return 1
  fi

  if [ -e "$stage" ] || [ -L "$stage" ]; then
    if [ ! -e "$target" ] && [ ! -L "$target" ] &&
       [ -f "$stage" ] && [ ! -L "$stage" ] &&
       verify_digest "$algorithm" "$expected" "$stage" "staged tool target" 2>/dev/null &&
      command_has_version "$stage" "$version"; then
      mv "$stage" "$target"
      direct_owner_write "$target" || return 1
    else
      rm -f "$stage"
    fi
  fi
}

install_direct_binary() {  # SOURCE TARGET ALGORITHM EXPECTED VERSION LABEL
  local source="$1" target="$2" algorithm="$3" expected="$4" version="$5" label="$6"
  local stage="$2.oh-my-setting-stage" backup="$2.oh-my-setting-backup"
  local had_target=0

  recover_direct_install "$target" "$algorithm" "$expected" "$version" || return 1
  if [ -e "$target" ] || [ -L "$target" ]; then
    { direct_target_owned "$target" || direct_target_claimed "$target"; } || {
      echo "error: refusing to overwrite user-owned tool target: $target" >&2
      echo "error: move that collision and rerun; it was not modified" >&2
      return 1
    }
    had_target=1
  fi

  install -m 0755 "$source" "$stage"
  if ! verify_digest "$algorithm" "$expected" "$stage" "$label staged payload" ||
     ! command_has_version "$stage" "$version"; then
    rm -f "$stage"
    echo "error: staged $label payload failed verification" >&2
    return 1
  fi
  [ "$had_target" = 0 ] || mv "$target" "$backup"
  if ! mv "$stage" "$target" ||
     ! verify_digest "$algorithm" "$expected" "$target" "$label installed payload" ||
     ! command_has_version "$target" "$version"; then
    rm -f "$stage"
    [ ! -e "$target" ] && [ ! -L "$target" ] || rm -f "$target"
    if [ "$had_target" = 1 ] && [ -e "$backup" ]; then
      mv "$backup" "$target"
    fi
    echo "error: $label install failed and the previous target was restored" >&2
    return 1
  fi
  if ! direct_owner_write "$target"; then
    rm -f "$target"
    if [ "$had_target" = 1 ] && [ -e "$backup" ]; then
      mv "$backup" "$target"
    fi
    echo "error: $label ownership record failed and the previous target was restored" >&2
    return 1
  fi
  rm -f "$backup"
}

restore_direct_pair_member() {  # TARGET HAD_TARGET BACKED_UP PUBLISHED
  local target="$1" had_target="$2" backed_up="$3" published="$4"
  local stage="$1.oh-my-setting-stage" backup="$1.oh-my-setting-backup"

  rm -f "$stage"
  [ "$published" = 0 ] || rm -f "$target"
  if [ "$backed_up" = 1 ]; then
    mv "$backup" "$target" || return 1
    direct_owner_write "$target" || return 1
  else
    rm -f "$backup"
    [ "$had_target" = 1 ] || rm -f "$target.oh-my-setting-managed"
  fi
}

install_uv_binaries() {  # SOURCE_UV SOURCE_UVX TARGET_UV TARGET_UVX SHA_UV SHA_UVX VERSION PLATFORM
  local source_uv="$1" source_uvx="$2" target_uv="$3" target_uvx="$4"
  local expected_uv="$5" expected_uvx="$6" version="$7" platform="$8"
  local stage_uv="$3.oh-my-setting-stage" stage_uvx="$4.oh-my-setting-stage"
  local backup_uv="$3.oh-my-setting-backup" backup_uvx="$4.oh-my-setting-backup"
  local had_uv=0 had_uvx=0 backed_uv=0 backed_uvx=0 published_uv=0 published_uvx=0
  local target

  # Recover the companion first: uvx resolves the adjacent uv command when its
  # own interrupted transaction is inspected.
  recover_direct_install "$target_uv" sha256 "$expected_uv" "$version" || return 1
  recover_direct_install "$target_uvx" sha256 "$expected_uvx" "$version" || return 1
  for target in "$target_uv" "$target_uvx"; do
    if [ -e "$target" ] || [ -L "$target" ]; then
      { direct_target_owned "$target" || direct_target_claimed "$target"; } || {
        echo "error: refusing to overwrite user-owned tool target: $target" >&2
        echo "error: move that collision and rerun; it was not modified" >&2
        return 1
      }
    fi
  done

  install -m 0755 "$source_uv" "$stage_uv" || return 1
  if ! install -m 0755 "$source_uvx" "$stage_uvx"; then
    rm -f "$stage_uv"
    return 1
  fi
  # uvx is a launcher. Keeping both staged names adjacent lets it verify the
  # exact staged uv instead of falling through to an older live installation.
  if ! verify_digest sha256 "$expected_uv" "$stage_uv" "uv $version ($platform) staged payload" ||
     ! verify_digest sha256 "$expected_uvx" "$stage_uvx" "uvx $version ($platform) staged payload" ||
     ! command_has_version "$stage_uv" "$version" ||
     ! command_has_version "$stage_uvx" "$version"; then
    rm -f "$stage_uv" "$stage_uvx"
    echo "error: staged uv/uvx $version ($platform) payloads failed verification" >&2
    return 1
  fi

  [ ! -e "$target_uv" ] && [ ! -L "$target_uv" ] || had_uv=1
  [ ! -e "$target_uvx" ] && [ ! -L "$target_uvx" ] || had_uvx=1
  if [ "$had_uv" = 1 ]; then
    mv "$target_uv" "$backup_uv" || return 1
    backed_uv=1
  fi
  if [ "$had_uvx" = 1 ]; then
    if ! mv "$target_uvx" "$backup_uvx"; then
      restore_direct_pair_member "$target_uv" "$had_uv" "$backed_uv" "$published_uv"
      rm -f "$stage_uvx"
      return 1
    fi
    backed_uvx=1
  fi
  if mv "$stage_uv" "$target_uv"; then published_uv=1; else
    restore_direct_pair_member "$target_uv" "$had_uv" "$backed_uv" "$published_uv"
    restore_direct_pair_member "$target_uvx" "$had_uvx" "$backed_uvx" "$published_uvx"
    return 1
  fi
  if mv "$stage_uvx" "$target_uvx"; then published_uvx=1; else
    restore_direct_pair_member "$target_uv" "$had_uv" "$backed_uv" "$published_uv"
    restore_direct_pair_member "$target_uvx" "$had_uvx" "$backed_uvx" "$published_uvx"
    return 1
  fi

  if ! verify_digest sha256 "$expected_uv" "$target_uv" "uv $version ($platform) installed payload" ||
     ! verify_digest sha256 "$expected_uvx" "$target_uvx" "uvx $version ($platform) installed payload" ||
     ! command_has_version "$target_uv" "$version" ||
     ! command_has_version "$target_uvx" "$version" ||
     ! direct_owner_write "$target_uv" ||
     ! direct_owner_write "$target_uvx"; then
    restore_direct_pair_member "$target_uv" "$had_uv" "$backed_uv" "$published_uv"
    restore_direct_pair_member "$target_uvx" "$had_uvx" "$backed_uvx" "$published_uvx"
    echo "error: uv/uvx install failed and previous targets were restored" >&2
    return 1
  fi
  rm -f "$backup_uv" "$backup_uvx"
}

install_antigravity() {
  local platform version url expected archive tmp source target source_expected
  platform="$(locked_platform)"
  version="$(tool_lock_get antigravity.version)"
  case "$platform" in windows-*) target="$HOME/.local/bin/agy.exe" ;; *) target="$HOME/.local/bin/agy" ;; esac
  if direct_locked_command_is_current agy "$target" "$version" &&
     { [ "$UPGRADE" != 1 ] || [ "${OH_MY_SETTING_UPGRADE_ANTIGRAVITY:-1}" = 0 ]; }; then
    echo "ok: agy $version"
    return 0
  fi
  url="$(tool_lock_get "antigravity.platforms.$platform.url")"
  expected="$(tool_lock_get "antigravity.platforms.$platform.sha512")"
  archive="$(tool_lock_get "antigravity.platforms.$platform.archive")"
  echo "installing Antigravity CLI $version ($platform)"
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/oms-agy.XXXXXX")"
  if ! download_locked "$url" "$tmp/payload" ||
     ! verify_digest sha512 "$expected" "$tmp/payload" "Antigravity $version ($platform)"; then
    rm -rf "$tmp"
    exit 1
  fi
  case "$archive" in
    tar.gz)
      if ! extract_locked tar.gz "$tmp/payload" "$tmp/extracted"; then
        rm -rf "$tmp"
        exit 1
      fi
      source="$tmp/extracted/antigravity"
      target="$HOME/.local/bin/agy"
      ;;
    binary)
      source="$tmp/payload"
      target="$HOME/.local/bin/agy.exe"
      ;;
    *) rm -rf "$tmp"; echo "error: unsupported Antigravity archive: $archive" >&2; exit 1 ;;
  esac
  [ -f "$source" ] || { rm -rf "$tmp"; echo "error: verified Antigravity payload has no binary" >&2; exit 1; }
  source_expected="$(sha256_file "$source")" || { rm -rf "$tmp"; exit 1; }
  ensure_local_bin_path
  if ! install_direct_binary "$source" "$target" sha256 "$source_expected" \
      "$version" "Antigravity $version ($platform)"; then
    rm -rf "$tmp"
    exit 1
  fi
  rm -rf "$tmp"

  if ! has_cmd agy; then
    echo "error: Antigravity CLI install completed but agy is not on PATH" >&2
    exit 1
  fi

  command_has_version agy "$version" || {
    echo "error: Antigravity install is not the locked version $version" >&2
    exit 1
  }
  echo "ok: agy $(agy --version)"
}

# The GitHub CLI is a harness dependency, not a convenience: ci-status.sh
# reads the last run through it, and PR work goes through it. Installed without sudo on purpose —
# a setup script that needs root is a setup script that gets run as root.
install_gh() {
  local platform version url expected archive tmp source target source_expected
  platform="$(locked_platform)"
  version="$(tool_lock_get gh.version)"
  case "$platform" in windows-*) target="$HOME/.local/bin/gh.exe" ;; *) target="$HOME/.local/bin/gh" ;; esac
  if direct_locked_command_is_current gh "$target" "$version"; then
    echo "ok: gh $version"
    return 0
  fi
  url="$(tool_lock_get "gh.platforms.$platform.url")"
  expected="$(tool_lock_get "gh.platforms.$platform.sha256")"
  archive="$(tool_lock_get "gh.platforms.$platform.archive")"
  echo "installing gh $version ($platform)"
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/oms-gh.XXXXXX")"
  if ! download_locked "$url" "$tmp/archive" ||
     ! verify_digest sha256 "$expected" "$tmp/archive" "gh $version ($platform)"; then
    rm -rf "$tmp"
    exit 1
  fi
  case "$archive" in
    tar.gz)
      if ! extract_locked tar.gz "$tmp/archive" "$tmp/extracted"; then
        rm -rf "$tmp"
        exit 1
      fi
      source="$(find "$tmp/extracted" -type f -path '*/bin/gh' -print | sed -n '1p')"
      target="$HOME/.local/bin/gh"
      ;;
    zip)
      if ! extract_locked zip "$tmp/archive" "$tmp/extracted"; then
        rm -rf "$tmp"
        exit 1
      fi
      source="$(find "$tmp/extracted" -type f \( -name gh -o -name gh.exe \) -print | sed -n '1p')"
      case "$platform" in windows-*) target="$HOME/.local/bin/gh.exe" ;; *) target="$HOME/.local/bin/gh" ;; esac
      ;;
    *) rm -rf "$tmp"; echo "error: unsupported gh archive: $archive" >&2; exit 1 ;;
  esac
  [ -n "$source" ] && [ -f "$source" ] || {
    rm -rf "$tmp"
    echo "error: verified gh archive did not contain the expected binary" >&2
    exit 1
  }
  source_expected="$(sha256_file "$source")" || { rm -rf "$tmp"; exit 1; }
  ensure_local_bin_path
  if ! install_direct_binary "$source" "$target" sha256 "$source_expected" \
      "$version" "gh $version ($platform)"; then
    rm -rf "$tmp"
    exit 1
  fi
  rm -rf "$tmp"

  if ! has_cmd gh; then
    echo "error: gh install completed but gh is not on PATH" >&2
    exit 1
  fi
  command_has_version gh "$version" || {
    echo "error: gh install is not the locked version $version" >&2
    exit 1
  }
  # Not reporting authentication here: install.sh runs doctor at the end, and
  # doctor already reads the same local credential and warns — alongside the
  # three provider CLIs, in one place instead of two saying the same thing.
  echo "ok: $(gh --version | head -n 1)"
}

ensure_uv() {
  local platform version url expected archive tmp source_uv source_uvx
  local target_uv target_uvx source_uv_expected source_uvx_expected
  export PATH="$HOME/.local/bin:$PATH"

  platform="$(locked_platform)"
  case "$platform" in
    windows-*) target_uv="$HOME/.local/bin/uv.exe"; target_uvx="$HOME/.local/bin/uvx.exe" ;;
    *) target_uv="$HOME/.local/bin/uv"; target_uvx="$HOME/.local/bin/uvx" ;;
  esac
  version="$(tool_lock_get uv.version)"
  if direct_locked_command_is_current uv "$target_uv" "$version" &&
     direct_locked_command_is_current uvx "$target_uvx" "$version"; then
    echo "ok: uv $(uv --version)"
    return 0
  fi
  url="$(tool_lock_get "uv.platforms.$platform.url")"
  expected="$(tool_lock_get "uv.platforms.$platform.sha256")"
  archive="$(tool_lock_get "uv.platforms.$platform.archive")"
  echo "installing uv $version ($platform)"
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/oms-uv.XXXXXX")"
  if ! download_locked "$url" "$tmp/archive" ||
     ! verify_digest sha256 "$expected" "$tmp/archive" "uv $version ($platform)"; then
    rm -rf "$tmp"
    exit 1
  fi
  if ! extract_locked "$archive" "$tmp/archive" "$tmp/extracted"; then
    rm -rf "$tmp"
    exit 1
  fi
  case "$platform" in
    windows-*)
      source_uv="$(find "$tmp/extracted" -type f -name uv.exe -print | sed -n '1p')"
      source_uvx="$(find "$tmp/extracted" -type f -name uvx.exe -print | sed -n '1p')"
      target_uv="$HOME/.local/bin/uv.exe"
      target_uvx="$HOME/.local/bin/uvx.exe"
      ;;
    *)
      source_uv="$(find "$tmp/extracted" -type f -name uv -print | sed -n '1p')"
      source_uvx="$(find "$tmp/extracted" -type f -name uvx -print | sed -n '1p')"
      target_uv="$HOME/.local/bin/uv"
      target_uvx="$HOME/.local/bin/uvx"
      ;;
  esac
  [ -n "$source_uv" ] && [ -f "$source_uv" ] &&
    [ -n "$source_uvx" ] && [ -f "$source_uvx" ] || {
      rm -rf "$tmp"
      echo "error: verified uv archive is missing uv or uvx" >&2
      exit 1
    }
  chmod +x "$source_uv" "$source_uvx"
  command_has_version "$source_uv" "$version" || {
    rm -rf "$tmp"
    echo "error: verified uv payload is not version $version" >&2
    exit 1
  }
  command_has_version "$source_uvx" "$version" || {
    rm -rf "$tmp"
    echo "error: verified uvx payload is not version $version" >&2
    exit 1
  }
  source_uv_expected="$(sha256_file "$source_uv")" || { rm -rf "$tmp"; exit 1; }
  source_uvx_expected="$(sha256_file "$source_uvx")" || { rm -rf "$tmp"; exit 1; }
  if ! install_uv_binaries "$source_uv" "$source_uvx" "$target_uv" "$target_uvx" \
      "$source_uv_expected" "$source_uvx_expected" "$version" "$platform"; then
    rm -rf "$tmp"
    exit 1
  fi
  rm -rf "$tmp"
  hash -r

  if ! has_cmd uv; then
    echo "error: uv install completed but uv is not on PATH" >&2
    exit 1
  fi
  command_has_version uv "$version" || {
    echo "error: uv install is not the locked version $version" >&2
    exit 1
  }

  echo "ok: uv $(uv --version)"
}

if [ "${BASH_SOURCE[0]}" != "$0" ]; then
  return 0 2>/dev/null || exit 0
fi

ensure_node
ensure_local_bin_path
ensure_writable_npm_global
preflight_npm_shim claude
preflight_npm_shim codex
preflight_npm_shim ntn
ensure_uv

install_npm_global claude
install_npm_global codex
install_npm_global ntn
install_antigravity
install_gh

write_npm_shim "claude"
write_npm_shim "codex"
write_npm_shim "ntn"

verify_resolved_npm_binary claude
verify_resolved_npm_binary codex
verify_resolved_npm_binary ntn

echo "tools: ok"
