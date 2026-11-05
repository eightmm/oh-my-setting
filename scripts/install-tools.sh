#!/usr/bin/env bash
set -euo pipefail

# Bootstrap tools remain pinned; providers resolve stable releases at install
# time. Downloaded bytes are verified before extraction or execution.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
TOOL_LOCK="${OH_MY_SETTING_TOOL_LOCK:-$ROOT/tools.lock.json}"
TOOL_LOCK_HELPER="$ROOT/scripts/lib/tool-lock.py"
NVM_DIR="${NVM_DIR:-$HOME/.nvm}"
UPGRADE=0
PROVIDERS_ONLY=0
# shellcheck source=scripts/lib/platform.sh
. "$ROOT/scripts/lib/platform.sh"

[ -x "$TOOL_LOCK_HELPER" ] || { echo "error: missing tool lock helper: $TOOL_LOCK_HELPER" >&2; exit 1; }
python3 "$TOOL_LOCK_HELPER" --lock "$TOOL_LOCK" validate >/dev/null || exit $?
tool_lock_get() {
  # The lock was validated above; the awk reader then serves each scalar
  # without one Python start per field (about 40 per install).
  oms_lock_scalar "$TOOL_LOCK" "$1" || {
    echo "error: unknown tool lock field: $1" >&2
    return 1
  }
}
NODE_VERSION="$(tool_lock_get node.version)"
NVM_VERSION="$(tool_lock_get nvm.version)"

usage() {
  cat <<'EOF'
Usage: install-tools.sh [--upgrade|--providers] [-h|--help]

Install Node (via nvm), uv, gh, and ntn from tools.lock.json.
Recognized vendor installations use claude update, codex update and agy update.
Only --providers refreshes installed Pi, using pi update self.
Their channels choose the version; OMS rejects downgrades.
Fresh Claude uses its checksum-verified native installer; native Windows Git
Bash keeps the npm path. Fresh Codex and agy keep their verified install paths.
--providers also refreshes installed optional npm CLIs to stable releases at
least 14 days old. Pi is never freshly installed by OMS.
An explicit OH_MY_SETTING_TOOL_LOCK retains reproducible pinned installs and
skips Pi refresh without changing its installation.

Nothing here needs root: npm globals use the active writable prefix or
~/.local, and direct release payloads such as gh are installed in ~/.local/bin.
EOF
}

[ "$#" -le 1 ] || { usage >&2; exit 2; }
case "${1:-}" in
  "") ;;
  --upgrade) UPGRADE=1 ;;
  --providers) UPGRADE=1; PROVIDERS_ONLY=1 ;;
  -h|--help) usage; exit 0 ;;
  *) echo "error: unknown option: $1" >&2; usage >&2; exit 2 ;;
esac

prepare_provider_versions() {
  local item snapshot
  local provider_args=()
  for item in "$@"; do
    if [ "$item" = claude ]; then preflight_claude_vendor || return 1; fi
  done
  [ -z "${OH_MY_SETTING_TOOL_LOCK:-}" ] || return 0
  for item in "$@"; do
    case "$item" in
      codex|claude|agy)
        vendor_provider_path "$item" >/dev/null && continue
        if [ "$item" = claude ] && claude_migration_pending; then continue; fi
        provider_args+=(--provider "$item") ;;
    esac
  done
  [ "${#provider_args[@]}" -gt 0 ] || return 0
  snapshot="$HOME/.local/share/oh-my-setting/provider-tools.lock.json"
  python3 "$ROOT/scripts/lib/provider-latest.py" --lock "$TOOL_LOCK" \
    --previous "$snapshot" --output "$snapshot" "${provider_args[@]}" >/dev/null || return $?
  TOOL_LOCK="$snapshot"
}

has_cmd() {
  command -v "$1" >/dev/null 2>&1
}

activate_provider_paths() {
  local node_dir="$NVM_DIR/versions/node/v$NODE_VERSION/bin"
  # Timers do not source shell startup files. Reuse installed tools without
  # executing nvm.sh or installing bootstrap dependencies during a refresh.
  if [ -x "$node_dir/node" ] && [ -x "$node_dir/npm" ]; then
    export PATH="$node_dir:$PATH"
  fi
  export PATH="$HOME/.local/bin:$PATH"
  hash -r
}

sha256_file() {
  oms_sha256_file "$1"
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
    sha256) actual="$(oms_sha256_file "$file")" || return 1 ;;
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
  oms_tool_platform
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

nvm_script_matches_lock() {
  local expected actual
  [ -s "$NVM_DIR/nvm.sh" ] || return 1
  expected="$(tool_lock_get nvm.script_sha256)"
  actual="$(oms_sha256_file "$NVM_DIR/nvm.sh" 2>/dev/null || true)"
  [ -n "$actual" ] && [ "$actual" = "$expected" ]
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
    if ! nvm_script_matches_lock; then
      echo "note: preserving existing NVM at $NVM_DIR; its nvm.sh is not the locked release, so OMS will not source it" >&2
    fi
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
  local managed_node_bin="$NVM_DIR/versions/node/v$NODE_VERSION/bin"
  if nvm_script_matches_lock; then
    load_nvm
    if ! has_cmd nvm; then
      echo "error: locked nvm is not loadable after setup" >&2
      exit 1
    fi
    nvm alias default "$NODE_VERSION"
    nvm use default
  else
    # The Node archive is independently version- and digest-verified. Use its
    # stable path without executing or replacing a user's older/foreign NVM.
    export PATH="$managed_node_bin:$PATH"
    hash -r
  fi
  has_cmd node && has_cmd npm && [ "$(node_exact_version)" = "$NODE_VERSION" ] || {
    echo "error: locked Node v$NODE_VERSION was installed but not activated" >&2
    exit 1
  }
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
  local marker="$1" phase="$2" had_package="$3" binary="$4" staged extra
  shift 4
  staged="$marker.stage.$$"
  {
    printf 'schema=1\n'
    printf 'phase=%s\n' "$phase"
    printf 'had_package=%s\n' "$had_package"
    printf 'binary=%s\n' "$binary"
    printf 'sentinels=1\n'
    for extra in "$@"; do printf 'extra_bin=%s\n' "$extra"; done
  } > "$staged"
  mv "$staged" "$marker"
}

npm_legacy_package_link() { # LINK PACKAGE_ROOT
  [ -L "$1" ] || return 1
  python3 - "$1" "$2" <<'PY_LINK'
import pathlib, sys
try:
    pathlib.Path(sys.argv[1]).resolve().relative_to(pathlib.Path(sys.argv[2]).resolve())
except (OSError, RuntimeError, ValueError):
    sys.exit(1)
PY_LINK
}

npm_transaction_restore() {  # PACKAGE_ROOT GLOBAL_BIN BINARY
  local package_root="$1" global_bin="$2" binary="$3"
  local marker="$1.oh-my-setting-transaction"
  local package_backup="$1.oh-my-setting-backup"
  local bin_backup="$2/.$3.oh-my-setting-backup"
  local phase had_package candidate saved extra extras legacy sentinel removed_package=0

  [ -f "$marker" ] || return 0
  [ "$(sed -n '1p' "$marker")" = schema=1 ] || {
    echo "error: invalid npm transaction marker requires manual review: $marker" >&2
    return 1
  }
  phase="$(sed -n 's/^phase=//p' "$marker")"
  had_package="$(sed -n 's/^had_package=//p' "$marker")"
  [ "$(sed -n 's/^binary=//p' "$marker")" = "$binary" ] || return 1
  case "$binary" in ''|*[!a-zA-Z0-9._-]*|.*|-*) return 1 ;; esac
  case "$phase:$had_package" in
    prepared:0|prepared:1|backed_up:0|backed_up:1) ;;
    *) echo "error: invalid npm transaction state: $marker" >&2; return 1 ;;
  esac

  extras="$(sed -n 's/^extra_bin=//p' "$marker")"
  for extra in $extras; do
    case "$extra" in ''|*[!a-zA-Z0-9._-]*|.*|-*) echo "error: invalid npm transaction binary" >&2; return 1 ;; esac
  done
  legacy=0
  if ! grep -Fxq 'sentinels=1' "$marker"; then
    legacy=1
    for sentinel in "$bin_backup"/.absent.*; do
      [ ! -e "$sentinel" ] || legacy=0
    done
  fi
  if [ "$phase" = backed_up ]; then
    if [ "$had_package" = 0 ] || [ -e "$package_backup" ] || [ -L "$package_backup" ]; then
      rm -rf "$package_root" || return 1
      removed_package=1
    fi
    for extra in "$binary" $extras; do
      for candidate in "$global_bin/$extra" "$global_bin/$extra.exe" \
          "$global_bin/$extra.cmd" "$global_bin/$extra.ps1"; do
        saved="$bin_backup/$(basename "$candidate")"
        if [ -e "$saved" ] || [ -L "$saved" ] || [ -e "$bin_backup/.absent.$(basename "$candidate")" ]; then
          [ ! -e "$candidate" ] && [ ! -L "$candidate" ] || rm -f "$candidate" || return 1
        elif [ "$legacy" = 1 ] && [ "$removed_package" = 1 ] && npm_legacy_package_link "$candidate" "$package_root"; then
          rm -f "$candidate" || return 1
        fi
      done
    done
  fi

  if [ -e "$package_backup" ] || [ -L "$package_backup" ]; then
    [ ! -e "$package_root" ] && [ ! -L "$package_root" ] || {
      echo "error: npm package and recovery backup both exist: $package_root" >&2
      return 1
    }
    mv "$package_backup" "$package_root" || return 1
  elif [ "$phase" = backed_up ] && [ "$had_package" = 1 ] && [ ! -d "$package_root" ]; then
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
      mv "$saved" "$candidate" || return 1
    done
    rm -f "$bin_backup"/.absent.* || return 1
    rmdir "$bin_backup" || return 1
  fi
  rm -f "$marker" || return 1
}

npm_transaction_commit() {  # PACKAGE_ROOT GLOBAL_BIN BINARY
  local package_root="$1" global_bin="$2" binary="$3"
  local marker="$package_root.oh-my-setting-transaction" staged="$package_root.oh-my-setting-transaction.stage.$$" phase
  case "$binary" in ''|*[!a-zA-Z0-9._-]*|.*|-*) return 1 ;; esac
  # Once committed, recovery must finish cleanup and never restore the old package.
  if [ -f "$marker" ]; then
    [ "$(sed -n '1p' "$marker")" = schema=1 ] || return 1
    [ "$(sed -n 's/^binary=//p' "$marker")" = "$binary" ] || return 1
    phase="$(sed -n 's/^phase=//p' "$marker")"
    case "$phase" in backed_up|committed) ;; *) return 1 ;; esac
    sed 's/^phase=.*/phase=committed/' "$marker" > "$staged" || return 1
    mv "$staged" "$marker" || return 1
  fi
  rm -rf "$package_root.oh-my-setting-backup" || return 1
  rm -rf "$global_bin/.$binary.oh-my-setting-backup" || return 1
  rm -f "$marker" || return 1
}

npm_transaction_recover() {  # NAME PACKAGE_ROOT GLOBAL_BIN BINARY
  local name="$1" package_root="$2" global_bin="$3" binary="$4"
  local marker="$2.oh-my-setting-transaction" phase
  [ -f "$marker" ] || return 0
  phase="$(sed -n 's/^phase=//p' "$marker")"
  if [ "$phase" = committed ] || { [ "$phase" = backed_up ] && npm_package_is_current "$name"; }; then
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
  local had_package=0 candidate extra saved
  shift 3

  [ ! -e "$marker" ] && [ ! -e "$package_backup" ] && [ ! -e "$bin_backup" ] || {
    echo "error: stale npm transaction state requires recovery before install" >&2
    return 1
  }
  mkdir -p "$(dirname "$package_root")" "$global_bin"
  if [ -e "$package_root" ] || [ -L "$package_root" ]; then had_package=1; fi
  npm_transaction_write "$marker" prepared "$had_package" "$binary" "$@" || return 1

  [ "$had_package" = 0 ] || mv "$package_root" "$package_backup" || return 1
  mkdir "$bin_backup" || return 1
  for extra in "$binary" "$@"; do
    for candidate in "$global_bin/$extra" "$global_bin/$extra.exe" \
        "$global_bin/$extra.cmd" "$global_bin/$extra.ps1"; do
      saved="$bin_backup/$(basename "$candidate")"
      if [ -e "$saved" ] || [ -L "$saved" ] || [ -e "$bin_backup/.absent.$(basename "$candidate")" ]; then
        continue
      fi
      if [ -e "$candidate" ] || [ -L "$candidate" ]; then
        mv "$candidate" "$saved" || return 1
      else
        : > "$bin_backup/.absent.$(basename "$candidate")" || return 1
      fi
    done
  done
  npm_transaction_write "$marker" backed_up "$had_package" "$binary" "$@" || {
    npm_transaction_restore "$package_root" "$global_bin" "$binary"
    return 1
  }
}

vendor_provider_path() { # PROVIDER [COMMAND_PATH]
  local candidate="${2:-}" home_physical resolved
  [ -n "$candidate" ] || candidate="$(command -v "$1" 2>/dev/null)" || return 1
  home_physical="$(cd "$HOME" && pwd -P)" || return 1
  resolved="$(python3 - "$1" "$candidate" "$home_physical" "${CODEX_HOME:-$HOME/.codex}" <<'PY_VENDOR' | tr -d '\r'
import os, sys
from pathlib import Path
try:
    name, candidate, home, codex = sys.argv[1:]
    home = Path(home)
    path = Path(candidate).resolve(strict=True)
    if not path.is_file() or not os.access(path, os.X_OK):
        raise ValueError()
    if name == "claude":
        parts = path.relative_to((home / ".local/share/claude/versions").resolve(strict=True)).parts
        valid = len(parts) == 1
    elif name == "codex":
        parts = path.relative_to((Path(codex) / "packages/standalone").resolve(strict=True)).parts
        valid = len(parts) == 4 and parts[0] == "releases" and parts[2:] == ("bin", "codex")
    elif name == "pi":
        root = (home / ".pi/agent").resolve(strict=True)
        path.relative_to(root)
        valid = (path == (root / "bin/pi").resolve(strict=True)
                 and (root / "install/current-version").is_file()
                 and (root / "install/releases").is_dir())
    elif name == "agy":
        launcher = home / ".local/bin/agy"
        if not launcher.exists():
            launcher = home / ".local/bin/agy.exe"
        valid = not launcher.is_symlink() and path == launcher.resolve(strict=True)
    else:
        valid = False
    if not valid:
        raise ValueError()
    print(path)
except (OSError, ValueError, RuntimeError):
    sys.exit(1)
PY_VENDOR
)" || return 1
  normalize_npm_path "$resolved"
}

native_claude() { vendor_provider_path claude >/dev/null; }
standalone_codex() { vendor_provider_path codex >/dev/null; }
native_antigravity() { vendor_provider_path agy >/dev/null; }
native_pi() { vendor_provider_path pi >/dev/null; }

provider_command_version() { # RESOLVED_COMMAND
  local output
  output="$("$1" --version 2>/dev/null | tr -d '\r')" || return 1
  printf '%s\n' "$output" | python3 -c '
import re, sys
version = re.search(r"(?<![0-9A-Za-z.+-])v?((?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)(?![0-9A-Za-z.+-])", sys.stdin.read())
if not version:
    sys.exit(1)
print(version[1])
' | tr -d '\r'
}

provider_version_not_older() { # ACTUAL PREVIOUS
  python3 - "$1" "$2" <<'PY_VERSION'
import re, sys
pattern = r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?"
def compare(left, right):
    a, b = re.fullmatch(pattern, left), re.fullmatch(pattern, right)
    if not a or not b:
        raise ValueError()
    an, bn = tuple(map(int, a.groups()[:3])), tuple(map(int, b.groups()[:3]))
    if an != bn:
        return (an > bn) - (an < bn)
    ar, br = a[4], b[4]
    if ar is None or br is None:
        return (ar is None) - (br is None)
    for x, y in zip(ar.split("."), br.split(".")):
        if x == y:
            continue
        if x.isdigit() and y.isdigit():
            return (int(x) > int(y)) - (int(x) < int(y))
        if x.isdigit() != y.isdigit():
            return -1 if x.isdigit() else 1
        return (x > y) - (x < y)
    return (len(ar.split(".")) > len(br.split("."))) - (len(ar.split(".")) < len(br.split(".")))
try:
    sys.exit(0 if compare(sys.argv[1], sys.argv[2]) >= 0 else 1)
except ValueError:
    sys.exit(1)
PY_VERSION
}

verify_vendor_provider() { # PROVIDER [PREVIOUS_VERSION]
  local name="$1" previous="${2:-}" resolved actual expected launcher=""
  hash -r
  if [ "$name" = claude ]; then launcher="$HOME/.local/bin/claude"; fi
  resolved="$(vendor_provider_path "$name" "$launcher")" && actual="$(provider_command_version "$resolved")" || {
    echo "error: $name no longer resolves to a working vendor layout; repair with the vendor installer" >&2
    return 1
  }
  if [ -n "${OH_MY_SETTING_TOOL_LOCK:-}" ]; then
    case "$name" in
      claude|codex) expected="$(tool_lock_get "npm.$name.version")" ;;
      agy) expected="$(tool_lock_get antigravity.version)" ;;
      *) echo "error: $name has no explicit lock entry; omit OH_MY_SETTING_TOOL_LOCK to use its vendor channel" >&2; return 1 ;;
    esac
    [ "$actual" = "$expected" ] || {
      echo "error: vendor $name is $actual, expected explicit pin $expected; use the vendor installer to restore the pin" >&2
      return 1
    }
  elif [ -n "$previous" ] && ! provider_version_not_older "$actual" "$previous"; then
    echo "error: vendor $name version went down ($previous to $actual); restore it with the vendor installer" >&2
    return 1
  fi
  echo "ok: $name $actual (vendor channel)"
}

refresh_vendor_provider() { # PROVIDER
  local name="$1" resolved previous expected digest
  if [ "$name" = pi ] && [ -n "${OH_MY_SETTING_TOOL_LOCK:-}" ]; then
    echo "note: pi refresh skipped (explicit tool lock; OMS does not install pi)"
    return 0
  fi
  resolved="$(vendor_provider_path "$name")" || return 1
  if [ "$name" = agy ] &&
     { [ -e "$resolved.oh-my-setting-managed" ] || [ -L "$resolved.oh-my-setting-managed" ]; }; then
    if [ -z "${OH_MY_SETTING_TOOL_LOCK:-}" ] &&
       [ -d "$HOME/.local" ] && [ ! -L "$HOME/.local" ] &&
       [ -d "$HOME/.local/bin" ] && [ ! -L "$HOME/.local/bin" ] &&
       direct_target_claimed "$resolved"; then
      # The vendor owns subsequent updates; an old lock-install digest is no longer authoritative.
      digest="$(oms_sha256_file "$resolved")" || return 1
      if [ "$(sed -n '1p' "$resolved.oh-my-setting-managed")" != "sha256=$digest" ]; then
        echo "warning: agy changed since OMS installed it; vendor self-update assumed; OMS no longer tracks its digest" >&2
      fi
      rm -f "$resolved.oh-my-setting-managed" || return 1
    elif [ -z "${OH_MY_SETTING_TOOL_LOCK:-}" ] || ! direct_target_owned "$resolved"; then
      echo "error: agy managed binary digest changed or ownership marker invalid; update was not run" >&2
      echo "error: restore the verified OMS binary and marker, or preserve both elsewhere and rerun to install the locked payload" >&2
      return 1
    fi
  fi
  previous="$(provider_command_version "$resolved")" || {
    echo "error: cannot read vendor $name version; repair with the vendor installer" >&2; return 1;
  }
  if [ -n "${OH_MY_SETTING_TOOL_LOCK:-}" ]; then
    case "$name" in
      claude)
        expected="$(tool_lock_get npm.claude.version)"
        "$resolved" install "$expected" </dev/null || return $? ;;
      *)
        # Neither updater accepts an exact pin; check before invoking anything mutable.
        verify_vendor_provider "$name" >/dev/null || return 1 ;;
    esac
  else
    case "$name" in
      pi) "$resolved" update self || return $? ;;
      *) "$resolved" update || return $? ;;
    esac
  fi
  verify_vendor_provider "$name" "$previous" || return 1
}

refresh_standalone_codex() { refresh_vendor_provider codex; }

newest_native_claude() {
  local home_physical
  home_physical="$(cd "$HOME" && pwd -P)" || return 1
  python3 - "$home_physical/.local/share/claude/versions" <<'PY_NATIVE' | tr -d '\r'
import os, re, sys
from pathlib import Path
try:
    root = Path(sys.argv[1]).resolve(strict=True)
    candidates = []
    for candidate in root.iterdir():
        if not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", candidate.name):
            continue
        if candidate.is_file() and candidate.resolve(strict=True).parent == root and os.access(candidate, os.X_OK):
            candidates.append((tuple(map(int, candidate.name.split("."))), candidate.resolve(strict=True)))
    if not candidates:
        sys.exit(1)
    print(max(candidates)[1])
except (OSError, ValueError, RuntimeError):
    sys.exit(1)
PY_NATIVE
}

claude_migration_pending() {
  npm_shim_owned "$HOME/.local/bin/claude" ||
    npm_shim_owned "$HOME/.local/bin/.claude.oh-my-setting-migration" || return 1
  newest_native_claude >/dev/null
}

preflight_claude_vendor() {
  local target="$HOME/.local/bin/claude" root identity
  oms_platform_is_windows && return 0
  if { [ -e "$target" ] || [ -L "$target" ]; } &&
     ! npm_shim_owned "$target" && ! vendor_provider_path claude "$target" >/dev/null; then
    if [ ! -L "$target" ] || ! has_cmd npm ||
       ! root="$(npm_global_root)" ||
       ! identity="$(optional_npm_identity "$root" "$target" claude)" ||
       [ "${identity%%$'\t'*}" != '@anthropic-ai/claude-code' ]; then
      echo "error: Claude launcher is not OMS-owned, vendor-native or in the active npm root; repair it with the vendor installer (it was not moved or overwritten)" >&2
      return 1
    fi
  fi
  if claude_migration_pending && ! claude_migration_source >/dev/null; then
    echo "error: cannot prove Claude migration ownership; restore the OMS npm copy or use the vendor installer" >&2
    return 1
  fi
  return 0
}

claude_migration_source() {
  local home_physical native shim="$HOME/.local/bin/claude"
  home_physical="$(cd "$HOME" && pwd -P)" || return 1
  if [ -e "$HOME/.local/bin/.claude.oh-my-setting-migration" ] ||
     [ -L "$HOME/.local/bin/.claude.oh-my-setting-migration" ]; then
    shim="$HOME/.local/bin/.claude.oh-my-setting-migration"
  fi
  npm_shim_owned "$shim" || return 1
  native="$(newest_native_claude)" || return 1
  python3 - "$home_physical" "$shim" "$native" <<'PY_MIGRATE' | tr -d '\r'
import json, os, shlex, sys
from pathlib import Path
try:
    home = Path(sys.argv[1])
    shim = Path(sys.argv[2])
    lines = shim.read_text().splitlines()
    command = shlex.split(lines[5])
    if len(lines) != 6 or len(command) != 3 or command[0] != "exec" or command[2] != "$@":
        raise ValueError()
    binary = Path(command[1])
    prefix = binary.parent.parent.resolve(strict=True)
    if binary.parent.name != "bin" or binary.name != "claude":
        raise ValueError()
    package = prefix / "lib/node_modules/@anthropic-ai/claude-code"
    package_present = package.exists() or package.is_symlink()
    if package_present:
        if package.resolve(strict=True) != package:
            raise ValueError()
        row = json.loads((package / "package.json").read_text())
        if row["name"] != "@anthropic-ai/claude-code":
            raise ValueError()
        bins = row["bin"]
        target = bins if isinstance(bins, str) else bins["claude"]
        actual = (package / target).resolve(strict=True)
        actual.relative_to(package)
        if binary.resolve(strict=True) != actual:
            raise ValueError()
        if not os.access(prefix / "bin/npm", os.X_OK):
            raise ValueError()
    else:
        # Cleanup may have completed before a kill left the parked shim behind.
        if shim.name != ".claude.oh-my-setting-migration":
            raise ValueError()
        native = (home / ".local/bin/claude").resolve(strict=True)
        parts = native.relative_to((home / ".local/share/claude/versions").resolve(strict=True)).parts
        if len(parts) != 1 or not native.is_file() or not os.access(native, os.X_OK):
            raise ValueError()
    print(sys.argv[3])
    print(prefix)
except (OSError, ValueError, KeyError, TypeError, IndexError, RuntimeError):
    sys.exit(1)
PY_MIGRATE
}

migrate_claude_vendor() {
  local source resolved prefix previous expected shim="$HOME/.local/bin/claude"
  local backup="$HOME/.local/bin/.claude.oh-my-setting-migration" previous_command
  source="$(claude_migration_source)" || {
    echo "error: cannot prove Claude shim npm prefix or find a native version; restore the OMS npm copy, or remove only the OMS shim and run the vendor installer" >&2
    return 1
  }
  resolved="${source%%$'\n'*}"
  prefix="${source#*$'\n'}"
  previous_command="$shim"
  if [ -e "$backup" ] || [ -L "$backup" ]; then
    if npm_shim_owned "$shim" ||
       { { [ -e "$shim" ] || [ -L "$shim" ]; } && ! vendor_provider_path claude "$shim" >/dev/null; }; then
      echo "error: conflicting Claude migration backup; preserve it and restore the OMS shim before retrying" >&2
      return 1
    fi
    previous_command="$backup"
  fi
  if ! previous="$(provider_command_version "$previous_command")"; then
    vendor_provider_path claude "$shim" >/dev/null || return 1
    previous=""
  fi
  provider_command_version "$resolved" >/dev/null || return 1
  expected=latest
  [ -z "${OH_MY_SETTING_TOOL_LOCK:-}" ] || expected="$(tool_lock_get npm.claude.version)"
  if [ "$previous_command" = "$shim" ]; then
    # The vendor preserves custom launchers. Move only the proven OMS shim;
    # its backup also lets a later lifecycle call resume an interrupted install.
    mv "$shim" "$backup" || return 1
  fi
  if ! vendor_provider_path claude "$shim" >/dev/null; then
    if ! "$resolved" install "$expected" </dev/null; then
      if [ ! -e "$shim" ] && [ ! -L "$shim" ]; then mv "$backup" "$shim" || return 1; fi
      echo "error: Claude migration installer failed; restore the OMS migration backup or rerun the vendor installer" >&2
      return 1
    fi
  fi
  verify_vendor_provider claude "$previous" || return 1
  # Keep the captured prefix until cleanup commits; a retry verifies the native
  # launcher directly and finishes cleanup without installing a second time.
  if [ -e "$prefix/lib/node_modules/@anthropic-ai/claude-code" ] ||
     [ -L "$prefix/lib/node_modules/@anthropic-ai/claude-code" ]; then
    "$prefix/bin/npm" uninstall -g --prefix "$prefix" --ignore-scripts --no-audit --no-fund -- @anthropic-ai/claude-code || {
      echo "error: native Claude installed; remove the former OMS npm copy with npm uninstall -g --prefix '$prefix' @anthropic-ai/claude-code" >&2
      return 1
    }
  fi
  hash -r
  npm_shim_owned "$backup" && rm -f "$backup" || {
    echo "error: Claude migration backup changed; preserve it and remove only the former OMS shim" >&2; return 1;
  }
}

install_native_claude() {
  local version platform base tmp checksum
  version="$(tool_lock_get npm.claude.version)"
  platform="$(locked_platform)" || return 1
  case "$platform" in
    linux-amd64) platform=linux-x64 ;;
    darwin-amd64) platform=darwin-x64 ;;
    linux-arm64|darwin-arm64) ;;
    *) echo "error: unsupported native Claude platform: $platform" >&2; return 1 ;;
  esac
  case "$platform" in
    linux-*)
      if { ldd --version 2>&1 || true; } | grep -qi musl; then platform="$platform-musl"; fi ;;
  esac
  base=https://downloads.claude.ai/claude-code-releases
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/oms-claude.XXXXXX")" || return 1
  if ! download_locked "$base/$version/manifest.json" "$tmp/manifest.json"; then rm -rf "$tmp"; return 1; fi
  checksum="$(python3 - "$tmp/manifest.json" "$platform" <<'PY_MANIFEST' | tr -d '\r'
import json, re, sys
try:
    value = json.load(open(sys.argv[1]))["platforms"][sys.argv[2]]["checksum"]
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{64}", value):
        raise ValueError()
    print(value)
except (OSError, ValueError, KeyError, TypeError):
    sys.exit(1)
PY_MANIFEST
)" || { rm -rf "$tmp"; echo "error: invalid Claude manifest checksum" >&2; return 1; }
  if ! download_locked "$base/$version/$platform/claude" "$tmp/claude" ||
     ! verify_digest sha256 "$checksum" "$tmp/claude" "Claude $version ($platform)"; then
    rm -rf "$tmp"; return 1
  fi
  if ! chmod +x "$tmp/claude"; then rm -rf "$tmp"; return 1; fi
  if ! "$tmp/claude" install "$version" </dev/null; then rm -rf "$tmp"; return 1; fi
  rm -rf "$tmp"
  verify_vendor_provider claude || return 1
  command_has_version "$(vendor_provider_path claude)" "$version" || {
    echo "error: native Claude install did not produce requested version $version" >&2; return 1;
  }
}

install_npm_global() {
  local name="$1"
  if [ "$name" = claude ]; then preflight_claude_vendor || return 1; fi
  if [ "$name" = claude ] && ! oms_platform_is_windows && claude_migration_pending; then
    migrate_claude_vendor; return $?
  fi
  case "$name" in
    claude|codex)
      if vendor_provider_path "$name" >/dev/null; then refresh_vendor_provider "$name"; return $?; fi ;;
  esac
  if [ "$name" = claude ] && ! oms_platform_is_windows; then
    if ! has_cmd claude && [ ! -e "$HOME/.local/bin/claude" ] && [ ! -L "$HOME/.local/bin/claude" ]; then
      install_native_claude; return $?
    fi
  fi
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
  if [ "$binary" = claude ]; then preflight_claude_vendor || return 1; fi
  case "$binary" in
    claude|codex) vendor_provider_path "$binary" >/dev/null && return 0 ;;
  esac
  if [ "$binary" = claude ] && ! oms_platform_is_windows; then
    if claude_migration_pending; then
      claude_migration_source >/dev/null || {
        echo "error: cannot prove Claude migration ownership; restore the OMS npm copy or use the vendor installer" >&2; return 1;
      }
      return 0
    fi
    if ! has_cmd claude && [ ! -e "$target" ] && [ ! -L "$target" ]; then return 0; fi
  fi
  global_bin="$(npm_global_bin_dir)" || return 1
  [ "$(normalize_npm_path "$global_bin")" != "$(normalize_npm_path "$HOME/.local/bin")" ] || return 0
  [ ! -e "$target" ] && [ ! -L "$target" ] && return 0
  npm_shim_owned "$target" && return 0
  echo "error: $target shadows the locked npm-managed $binary command" >&2
  echo "error: move the user-owned shadow and rerun; it was not overwritten" >&2
  return 1
}

write_npm_shim() {
  local binary="$1"
  case "$binary" in
    claude|codex) vendor_provider_path "$binary" >/dev/null && return 0 ;;
  esac
  local actual
  local node_bin
  local target="$HOME/.local/bin/$binary"

  actual="$(managed_npm_binary "$binary")" || {
    echo "error: no npm-managed binary found for $binary" >&2
    return 1
  }
  if [ "$(normalize_npm_path "$(dirname "$actual")")" = "$(normalize_npm_path "$HOME/.local/bin")" ]; then return 0; fi
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
  case "$name" in
    claude|codex)
      if vendor_provider_path "$name" >/dev/null; then verify_vendor_provider "$name"; return $?; fi ;;
  esac
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
  digest="$(oms_sha256_file "$target")" || return 1
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
  digest="$(oms_sha256_file "$target")" || return 1
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
  local platform="" version url expected archive tmp source target source_expected native recovering=0
  native="$(vendor_provider_path agy)" || native=""
  if [ -n "$native" ]; then
    target="$native"
  else
    platform="$(locked_platform)"
    case "$platform" in windows-*) target="$HOME/.local/bin/agy.exe" ;; *) target="$HOME/.local/bin/agy" ;; esac
  fi
  if [ -e "$target.oh-my-setting-backup" ] || [ -L "$target.oh-my-setting-backup" ] ||
     [ -e "$target.oh-my-setting-stage" ] || [ -L "$target.oh-my-setting-stage" ]; then
    if ! direct_target_claimed "$target" &&
       ! direct_target_claimed "$target.oh-my-setting-backup" "$target" &&
       ! direct_target_claimed "$target.oh-my-setting-stage" "$target"; then
      echo "error: unowned agy transaction residue; preserve it for manual review before retrying" >&2
      return 1
    fi
    recovering=1
  elif [ -n "$native" ]; then
    refresh_vendor_provider agy
    return $?
  fi
  [ -n "$platform" ] || platform="$(locked_platform)"
  version="$(tool_lock_get antigravity.version)"
  if [ "$recovering" = 0 ] && direct_locked_command_is_current agy "$target" "$version" &&
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
  source_expected="$(oms_sha256_file "$source")" || { rm -rf "$tmp"; exit 1; }
  if [ "$recovering" = 1 ]; then
    recover_direct_install "$target" sha256 "$source_expected" "$version" || { rm -rf "$tmp"; return 1; }
    if direct_target_owned "$target" && command_has_version "$target" "$version" && native_antigravity; then
      rm -rf "$tmp"
      refresh_vendor_provider agy
      return $?
    fi
  fi
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
  if [ -z "${OH_MY_SETTING_TOOL_LOCK:-}" ]; then
    rm -f "$target.oh-my-setting-managed" || return 1
  fi
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
  source_expected="$(oms_sha256_file "$source")" || { rm -rf "$tmp"; exit 1; }
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
  echo "ok: $(gh --version | sed -n '1p')"
}

ensure_uv() {
  local platform version url expected archive tmp source_uv source_uvx
  local target_uv target_uvx source_uv_expected source_uvx_expected uv_bin_dir
  uv_bin_dir="${OMS_UV_BIN_DIR:-$HOME/.local/bin}"
  export PATH="$uv_bin_dir:$HOME/.local/bin:$PATH"
  mkdir -p "$uv_bin_dir"

  platform="$(locked_platform)"
  case "$platform" in
    windows-*) target_uv="$uv_bin_dir/uv.exe"; target_uvx="$uv_bin_dir/uvx.exe" ;;
    *) target_uv="$uv_bin_dir/uv"; target_uvx="$uv_bin_dir/uvx" ;;
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
      target_uv="$uv_bin_dir/uv.exe"
      target_uvx="$uv_bin_dir/uvx.exe"
      ;;
    *)
      source_uv="$(find "$tmp/extracted" -type f -name uv -print | sed -n '1p')"
      source_uvx="$(find "$tmp/extracted" -type f -name uvx -print | sed -n '1p')"
      target_uv="$uv_bin_dir/uv"
      target_uvx="$uv_bin_dir/uvx"
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
  source_uv_expected="$(oms_sha256_file "$source_uv")" || { rm -rf "$tmp"; exit 1; }
  source_uvx_expected="$(oms_sha256_file "$source_uvx")" || { rm -rf "$tmp"; exit 1; }
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

# Resolve the actual executable and require its package identity to match its
# physical location directly under the global root, including scoped names.
optional_npm_identity() { # ROOT BINARY
  local prefix="" windows=0
  if oms_platform_is_windows; then
    windows=1
    prefix="$(npm_global_prefix)" || return 1
  fi
  python3 - "$1" "$2" "${3:-}" "$windows" "$prefix" <<'PY_ID' | tr -d '\r'
import json, pathlib, re, sys
try:
    root = pathlib.Path(sys.argv[1]).resolve(strict=True)
    binary = pathlib.Path(sys.argv[2]).resolve(strict=True)
    is_shim = False
    try:
        parts = binary.relative_to(root).parts
        count = 2 if parts and parts[0].startswith('@') else 1
        if len(parts) <= count:
            raise ValueError('binary is outside a package')
        package = root.joinpath(*parts[:count])
    except ValueError:
        is_shim = True
        if sys.argv[4] != '1' or binary.parent != pathlib.Path(sys.argv[5]).resolve(strict=True):
            raise
        if binary.name.lower() not in (sys.argv[3].lower(), sys.argv[3].lower() + '.cmd'):
            raise ValueError('not the requested shim')
        shim = binary.read_text(encoding='utf-8').replace('\\', '/').lower()
        candidates = list(root.glob('*/package.json')) + list(root.glob('@*/*/package.json'))
        matches = []
        for manifest in candidates:
            try:
                data = json.loads(manifest.read_text(encoding='utf-8'))
                name = data['name']
                bins = data.get('bin', {})
                if isinstance(bins, str):
                    bins = {name.split('/')[-1]: bins}
                target = bins.get(sys.argv[3])
                if not isinstance(target, str):
                    continue
                pkg = manifest.parent
                target_path = (pkg / target).resolve(strict=True)
                target_path.relative_to(pkg.resolve(strict=True))
                rel = target_path.relative_to(root).as_posix().lower()
                needle = '"$basedir/node_modules/' + rel + '"'
                cmd_needle = '"%dp0%/node_modules/' + rel + '"'
                lines = [line.strip() for line in shim.splitlines()]
                shell_line = re.compile(r'^exec (?:"\$basedir/node(?:\.exe)?"|node)\s+' + re.escape(needle) + r'\s+"\$@"$')
                cmd_line = re.compile(r'^endlocal & goto #_undefined_# 2>nul \|\| title %comspec% & "%_prog%"\s+' + re.escape(cmd_needle) + r'\s+%\*$')
                if any(shell_line.fullmatch(line) or cmd_line.fullmatch(line) for line in lines):
                    matches.append(pkg)
            except (OSError, ValueError, KeyError, TypeError, AttributeError):
                continue
        if len(matches) != 1:
            raise ValueError('shim does not identify one package')
        package = matches[0]
        parts = package.relative_to(root).parts
        count = len(parts)
    if package.resolve(strict=True) != package:
        raise ValueError('package is not physically under root')
    row = json.loads((package / 'package.json').read_text(encoding='utf-8'))
    name, version = row['name'], row['version']
    if (name != '/'.join(parts[:count]) or len(name) > 214 or name in ('node_modules', 'favicon.ico')
            or not re.fullmatch(r'(?:@[a-z0-9][a-z0-9._-]*/)?[a-z0-9][a-z0-9._-]*', name)):
        raise ValueError('invalid package identity')
    bins = row.get('bin', {})
    if isinstance(bins, str):
        bins = {name.split('/')[-1]: bins}
    target = bins.get(sys.argv[3])
    if not isinstance(target, str):
        raise ValueError('missing declared binary')
    target_path = (package / target).resolve(strict=True)
    target_path.relative_to(package)
    if not is_shim and binary != target_path:
        raise ValueError('binary does not match package declaration')
    if not isinstance(version, str) or not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?', version):
        raise ValueError('invalid installed version')
    print(name + '\t' + version)
except (OSError, ValueError, KeyError, TypeError, AttributeError):
    sys.exit(1)
PY_ID
}

optional_npm_target() { # PACKAGE INSTALLED
  local versions
  versions="$(npm view --json -- "$1" versions)" || return 1
  npm view --json -- "$1" time | python3 -c '
import datetime, json, re, sys
try:
    rows = json.load(sys.stdin)
    published_versions = json.loads(sys.argv[2])
    if not isinstance(published_versions, list):
        published_versions = [published_versions]
    installed_text = sys.argv[1].split("+", 1)[0]
    installed = tuple(map(int, installed_text.split("-", 1)[0].split(".")))
    installed_prerelease = "-" in installed_text
    cutoff = datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(days=14)
    versions = []
    for version, date in rows.items():
        if version not in published_versions:
            continue
        if not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:\+[0-9A-Za-z.-]+)?", version):
            continue
        try:
            published = datetime.datetime.fromisoformat(date.replace("Z", "+00:00"))
            number = tuple(map(int, version.split("+", 1)[0].split(".")))
            if published.tzinfo and published <= cutoff and (number > installed or (number == installed and installed_prerelease)):
                versions.append((number, version))
        except (ValueError, TypeError, OverflowError):
            continue
    if versions:
        print(max(versions)[1])
except (ValueError, TypeError, AttributeError):
    sys.exit(1)
' "$2" "$versions" | tr -d '\r'
}

optional_npm_extra_bins() { # PACKAGE_ROOT PRIMARY_BINARY
  python3 - "$1" "$2" <<'PY_BINS' | tr -d '\r'
import json, pathlib, re, sys
try:
    row = json.loads((pathlib.Path(sys.argv[1]) / 'package.json').read_text(encoding='utf-8'))
    bins = row.get('bin', {})
    if isinstance(bins, str):
        bins = {row['name'].split('/')[-1]: bins}
    if not isinstance(bins, dict) or sys.argv[2] not in bins:
        raise ValueError
    for name in bins:
        if not isinstance(name, str) or not re.fullmatch(r'[a-zA-Z0-9][a-zA-Z0-9._-]*', name):
            raise ValueError
    for name in bins:
        if name != sys.argv[2]:
            print(name)
except (OSError, ValueError, TypeError, KeyError, AttributeError):
    sys.exit(1)
PY_BINS
}

optional_npm_recover() { # ROOT GLOBAL_BIN
  local root="$1" global_bin="$2" marker package_root binary phase
  for marker in "$root"/*.oh-my-setting-transaction "$root"/@*/*.oh-my-setting-transaction; do
    [ -f "$marker" ] || continue
    grep -q '^extra_bin=' "$marker" || continue
    package_root="${marker%.oh-my-setting-transaction}"
    binary="$(sed -n 's/^binary=//p' "$marker")"
    phase="$(sed -n 's/^phase=//p' "$marker")"
    if [ "$phase" = committed ]; then
      npm_transaction_commit "$package_root" "$global_bin" "$binary" || return 1
      continue
    fi
    if ! npm_transaction_restore "$package_root" "$global_bin" "$binary"; then
      echo "warning: $binary: interrupted npm restoration failed" >&2
      return 1
    fi
    hash -r
    if ! optional_npm_identity "$root" "$(command -v "$binary" 2>/dev/null)" "$binary" >/dev/null; then
      echo "warning: $binary: restored CLI could not be verified" >&2
      return 1
    fi
  done
}

refresh_optional_providers() {
  local provider binary resolved root prefix identity package previous target current before
  local global_bin package_root extra_bins path_entry old_ifs recovery_needed
  if native_pi; then refresh_vendor_provider pi || return $?; fi
  [ -z "${OH_MY_SETTING_TOOL_LOCK:-}" ] || return 0
  # shellcheck source=scripts/lib/provider-registry.sh
  . "$ROOT/scripts/lib/provider-registry.sh"
  local optional_recovered=0
  for provider in $(oms_provider_supported_names); do
    case "$provider" in codex|claude|antigravity) continue ;; esac
    binary="$(oms_provider_binary "$provider")" || continue
    resolved="$(command -v "$binary" 2>/dev/null)" || resolved=""
    if [ -z "$resolved" ]; then
      # Backing up an npm transaction temporarily removes its command. Discover
      # that residue without invoking npm on a healthy vendor-only refresh.
      recovery_needed=0
      old_ifs="$IFS"
      IFS=:
      for path_entry in ${PATH:-}; do
        [ -n "$path_entry" ] || path_entry=.
        if [ -d "$path_entry/.$binary.oh-my-setting-backup" ] || [ -L "$path_entry/$binary" ]; then
          recovery_needed=1
          break
        fi
      done
      IFS="$old_ifs"
      if [ "$recovery_needed" = 1 ] && has_cmd npm && [ "$(id -u)" != 0 ]; then
        root="$(npm_global_root)" && global_bin="$(npm_global_bin_dir)" || return 1
        optional_npm_recover "$root" "$global_bin" || return 1
        optional_recovered=1
        resolved="$(command -v "$binary" 2>/dev/null)" || resolved=""
      fi
    fi
    [ -n "$resolved" ] || continue
    if [ "$binary" = pi ] && native_pi; then continue; fi
    if ! has_cmd npm || [ "$(id -u)" = 0 ]; then
      echo "$provider: skipped (npm unavailable or running as root)"
      continue
    fi
    root="$(npm_global_root)" || root=""
    prefix="$(npm_global_prefix)" || prefix=""
    if [ -z "$root" ] || [ -z "$prefix" ]; then
      echo "warning: $provider: npm global paths unavailable" >&2
      continue
    fi
    if [ "$optional_recovered" = 0 ]; then
      global_bin="$(npm_global_bin_dir)" || return 1
      optional_npm_recover "$root" "$global_bin" || return 1
      optional_recovered=1
    fi
    identity="$(optional_npm_identity "$root" "$resolved" "$binary")" || {
      echo "$provider: skipped (binary is not in a global npm package)"
      continue
    }
    if [ ! -w "$prefix" ] || [ ! -w "$root" ]; then
      echo "warning: $provider: npm global prefix or root is not writable" >&2
      continue
    fi
    package="${identity%%$'\t'*}"
    previous="${identity#*$'\t'}"
    target="$(optional_npm_target "$package" "$previous")" || {
      echo "warning: $provider: npm release lookup failed" >&2
      continue
    }
    [ -n "$target" ] || { echo "$provider: no eligible newer release"; continue; }
    before="$(python3 -c 'import datetime; print((datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(days=14)).strftime("%Y-%m-%dT%H:%M:%SZ"))' | tr -d '\r')" || continue
    global_bin="$(npm_global_bin_dir)" || { echo "warning: $provider: npm bin path unavailable" >&2; continue; }
    package_root="$root/$package"
    extra_bins="$(optional_npm_extra_bins "$package_root" "$binary")" || {
      echo "warning: $provider: invalid package bin declarations" >&2; continue;
    }
    if [ -e "$package_root.oh-my-setting-transaction" ] || [ -e "$package_root.oh-my-setting-backup" ] ||
       [ -e "$global_bin/.$binary.oh-my-setting-backup" ]; then
      echo "warning: $provider: stale npm backup requires recovery" >&2
      return 1
    fi
    if ! npm_transaction_begin "$package_root" "$global_bin" "$binary" "$binary" $extra_bins; then
      npm_transaction_restore "$package_root" "$global_bin" "$binary" || return 1
      echo "warning: $provider: could not back up package" >&2
      continue
    fi
    if npm install -g --no-fund --no-audit "--before=$before" -- "$package@$target" &&
       current="$(optional_npm_identity "$root" "$(command -v "$binary")" "$binary")" &&
       [ "$current" = "$(printf '%s\t%s' "$package" "$target")" ]; then
      npm_transaction_commit "$package_root" "$global_bin" "$binary" || return 1
      echo "$provider: updated to $target"
    else
      if ! npm_transaction_restore "$package_root" "$global_bin" "$binary"; then
        echo "warning: $provider: restoration failed" >&2
        return 1
      fi
      hash -r
      current="$(optional_npm_identity "$root" "$(command -v "$binary" 2>/dev/null)" "$binary")" || current=""
      if [ "$current" != "$(printf '%s\t%s' "$package" "$previous")" ]; then
        echo "warning: $provider: restored CLI could not be verified" >&2
        return 1
      fi
      echo "warning: $provider: refresh failed; previous package restored" >&2
    fi
  done
}

if [ "${BASH_SOURCE[0]}" != "$0" ]; then
  return 0 2>/dev/null || exit 0
fi

[ "${OMS_HARNESS_CHILD:-0}" != 1 ] || {
  echo "error: a harness child cannot install provider tools" >&2; exit 2;
}
# shellcheck source=scripts/lib/install-lifecycle-lock.sh
. "$ROOT/scripts/lib/install-lifecycle-lock.sh"
trap oms_install_lifecycle_lock_release EXIT
oms_install_lifecycle_lock_acquire_or_borrow "refresh tools" || exit $?

if [ "$PROVIDERS_ONLY" = 1 ]; then
  activate_provider_paths
  providers=()
  for item in codex claude agy; do
    if has_cmd "$item" || { [ "$item" = claude ] && claude_migration_pending; }; then
      providers+=("$item")
    fi
  done
  [ "${#providers[@]}" -gt 0 ] || echo "providers: none installed"
  if [ "${#providers[@]}" -gt 0 ]; then
    prepare_provider_versions ${providers[@]+"${providers[@]}"}
  fi
  for item in ${providers[@]+"${providers[@]}"}; do
    if [ "$item" = agy ]; then
      install_antigravity
    elif [ "$item" = claude ] && claude_migration_pending; then
      migrate_claude_vendor
    elif vendor_provider_path "$item" >/dev/null; then
      refresh_vendor_provider "$item"
    else
      has_cmd npm || { echo "error: npm is required to update $item" >&2; exit 1; }
      ensure_writable_npm_global
      preflight_npm_shim "$item"
      install_npm_global "$item"
      write_npm_shim "$item"
      verify_resolved_npm_binary "$item"
    fi
  done
  refresh_optional_providers
  echo "providers: ok"
  exit 0
fi

prepare_provider_versions codex claude agy
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
