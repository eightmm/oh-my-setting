#!/usr/bin/env bash

# Shared, project-neutral Python runtime for OMS itself. Project environments
# remain untouched: activation only shadows python/python3 for the OMS process
# tree and deliberately does not set VIRTUAL_ENV or UV_PROJECT_ENVIRONMENT.

# Pins come from the awk lock reader: tool-lock.py cost one interpreter start
# per field, nine Python starts per activation (350ms on `oms inbox`).
if ! command -v oms_lock_scalar >/dev/null 2>&1 && [ -f "${BASH_SOURCE[0]%/*}/platform.sh" ]; then
  # shellcheck source=scripts/lib/platform.sh
  . "${BASH_SOURCE[0]%/*}/platform.sh"
fi

oms_python_runtime_root() {
  printf '%s\n' "${OMS_PYTHON_RUNTIME_ROOT:-$HOME/.local/share/oh-my-setting/python-runtime}"
}

oms_python_runtime_fail() {
  echo "error: OMS Python runtime: $*" >&2
  return 1
}

oms_python_runtime_version_valid() {
  local value="$1" old_ifs="$IFS"
  case "$value" in *[!0-9.]*|.*|*.|*..*) return 1 ;; esac
  IFS=.
  # shellcheck disable=SC2086
  set -- $value
  IFS="$old_ifs"
  [ "$#" -eq 3 ] && [ -n "$1" ] && [ -n "$2" ] && [ -n "$3" ]
}

oms_python_runtime_root_owned() {
  local root="$1" marker="$1/.oh-my-setting-python-runtime"
  [ -d "$root" ] && [ ! -L "$root" ] && [ -f "$marker" ] &&
    [ ! -L "$marker" ] && [ "$(sed -n '1p' "$marker" 2>/dev/null)" = schema=1 ] &&
    [ "$(sed -n '2p' "$marker" 2>/dev/null)" = owner=oh-my-setting ]
}

oms_python_runtime_prepare_root() {
  local root="$1" marker="$1/.oh-my-setting-python-runtime"
  [ ! -L "$root" ] || oms_python_runtime_fail "root is a symbolic link: $root" || return
  if [ -e "$root" ]; then
    [ -d "$root" ] || oms_python_runtime_fail "root is not a directory: $root" || return
    if [ ! -f "$marker" ]; then
      [ -z "$(ls -A "$root" 2>/dev/null)" ] ||
        oms_python_runtime_fail "refusing to claim a non-empty unowned root: $root" || return
    elif ! oms_python_runtime_root_owned "$root"; then
      oms_python_runtime_fail "ownership marker is invalid: $marker" || return
    fi
  else
    mkdir -p "$root" || return
  fi
  local child
  for child in envs managed launchers bin; do
    [ ! -L "$root/$child" ] && { [ ! -e "$root/$child" ] || [ -d "$root/$child" ]; } ||
      oms_python_runtime_fail "invalid runtime directory: $root/$child" || return
  done
  [ ! -L "$root/current" ] || oms_python_runtime_fail "current is a symbolic link" || return
  if [ ! -f "$marker" ]; then
    (
      umask 077
      printf 'schema=1\n'
      printf 'owner=oh-my-setting\n'
    ) > "$marker.tmp.$$"
    mv "$marker.tmp.$$" "$marker"
  fi
}

oms_python_runtime_env_owned() {
  local env_dir="$1" version="$2" marker="$1/.oh-my-setting-python-env"
  [ -d "$env_dir" ] && [ ! -L "$env_dir" ] && [ -f "$marker" ] &&
    [ ! -L "$marker" ] && [ "$(sed -n '1p' "$marker" 2>/dev/null)" = schema=1 ] &&
    [ "$(sed -n '2p' "$marker" 2>/dev/null)" = "version=$version" ]
}

oms_python_runtime_env_python() {
  local env_dir="$1" candidate
  for candidate in "$env_dir/bin/python3" "$env_dir/bin/python" \
      "$env_dir/Scripts/python.exe"; do
    if [ -x "$candidate" ]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  return 1
}

oms_python_runtime_python_matches() {
  local python="$1" expected="$2"
  "$python" -I -c 'import sys
expected = tuple(int(value) for value in sys.argv[1].split("."))
raise SystemExit(0 if sys.version_info[:3] == expected else 1)' "$expected" \
    >/dev/null 2>&1
}

oms_python_runtime_locked_version() {
  local version
  [ -f "$1/tools.lock.json" ] || return 1
  version="$(oms_lock_scalar "$1/tools.lock.json" python.version 2>/dev/null)" || return 1
  oms_python_runtime_version_valid "$version" || return 1
  printf '%s\n' "$version"
}

oms_python_runtime_matches_lock() {
  local root version
  root="$(oms_python_runtime_root)"
  version="$(oms_python_runtime_locked_version "$1")" || return
  [ ! -L "$root/launchers" ] && [ ! -L "$root/launchers/$version" ] &&
    [ -x "$root/launchers/$version/python3" ] &&
    oms_python_runtime_locked_python "$1" >/dev/null 2>&1
}

oms_python_runtime_locked_python() {
  local root version env_dir python
  root="$(oms_python_runtime_root)"
  oms_python_runtime_root_owned "$root" && [ ! -L "$root/envs" ] || return 1
  version="$(oms_python_runtime_locked_version "$1")" || return 1
  env_dir="$root/envs/$version"
  oms_python_runtime_env_owned "$env_dir" "$version" || return 1
  python="$(oms_python_runtime_env_python "$env_dir")" || return 1
  oms_python_runtime_python_matches "$python" "$version" || return 1
  printf '%s\n' "$python"
}

oms_python_runtime_write_launcher() {  # ROOT VERSION NAME [DIR]
  local root="$1" version="$2" name="$3" dir="${4:-launchers/$2}" up target stage
  # bin/ holds the one version-free path a managed python3 shim can name.
  case "$dir" in bin) up=.. ;; *) up=../.. ;; esac
  target="$root/$dir/$name"
  [ ! -L "$root/$dir" ] || return 1
  mkdir -p "$root/$dir"
  stage="$(mktemp "$root/$dir/.$name.XXXXXX")" || return
  {
    printf '#!/usr/bin/env bash\nversion=%s\nup=%s\n' "$version" "$up"
    cat <<'EOF_LAUNCHER'
set -euo pipefail
# Every managed python3 call runs this: no dirname process per interpreter start.
case "${BASH_SOURCE[0]}" in */*) here="${BASH_SOURCE[0]%/*}" ;; *) here=. ;; esac
root="$(cd "$here/$up" && pwd -P)"
unset PYTHONHOME
for candidate in "$root/envs/$version/bin/python3" \
    "$root/envs/$version/bin/python" "$root/envs/$version/Scripts/python.exe"; do
  if [ -x "$candidate" ]; then exec "$candidate" "$@"; fi
done
echo "error: OMS Python runtime is incomplete: $root/envs/$version" >&2
exit 1
EOF_LAUNCHER
  } > "$stage"
  chmod 0755 "$stage"
  mv "$stage" "$target"
}

# The private runtime is always a uv-managed download into its own root. A
# user's interpreter choices are for their projects: UV_PYTHON_PREFERENCE=
# only-system made `uv venv --managed-python` refuse and UV_PYTHON_DOWNLOADS=
# never refused the install. Network settings (proxy, mirror, offline, cache)
# stay the user's. Certificates come from the system store, as curl's do, so a
# network whose TLS proxy the host trusts also works for this download.
oms_python_runtime_uv() {  # oms_python_runtime_uv ROOT UV ARGS...
  local root="$1"
  shift
  env -u UV_PYTHON_PREFERENCE -u UV_NO_MANAGED_PYTHON -u UV_MANAGED_PYTHON \
    -u UV_PYTHON -u UV_SYSTEM_PYTHON -u UV_CONFIG_FILE -u UV_PROJECT -u UV_WORKING_DIR \
    UV_PYTHON_DOWNLOADS=manual UV_SYSTEM_CERTS="${UV_SYSTEM_CERTS:-1}" \
    UV_PYTHON_INSTALL_DIR="$root/managed" "$@"
}

oms_python_runtime_ensure_locked() {
  local uv="$1" version="$2" root env_dir stage python current_stage
  [ -x "$uv" ] || oms_python_runtime_fail "uv is not executable: $uv" || return
  oms_python_runtime_version_valid "$version" ||
    oms_python_runtime_fail "invalid locked version: $version" || return
  root="$(oms_python_runtime_root)"
  oms_python_runtime_prepare_root "$root" || return
  env_dir="$root/envs/$version"

  if oms_python_runtime_env_owned "$env_dir" "$version"; then
    python="$(oms_python_runtime_env_python "$env_dir" 2>/dev/null || true)"
    if [ -n "$python" ] && oms_python_runtime_python_matches "$python" "$version"; then
      current_stage="$root/current.tmp.$$"
      printf '%s\n' "$version" > "$current_stage"
      mv "$current_stage" "$root/current"
      oms_python_runtime_write_launcher "$root" "$version" python3
      oms_python_runtime_write_launcher "$root" "$version" python
      oms_python_runtime_write_launcher "$root" "$version" python3 bin
      return 0
    fi
    oms_python_runtime_fail "existing environment is invalid; preserved for repair: $env_dir" || return
  elif [ -e "$env_dir" ] || [ -L "$env_dir" ]; then
    oms_python_runtime_fail "refusing to replace an unowned environment: $env_dir" || return
  fi

  mkdir -p "$root/envs" "$root/managed"
  stage="$(mktemp -d "$root/envs/.stage.XXXXXX")" || return

  echo "installing OMS Python $version (private uv runtime)"
  oms_python_runtime_uv "$root" \
    "$uv" python install --install-dir "$root/managed" --no-bin --no-config \
      "$version" || { rmdir "$stage"; return 1; }
  oms_python_runtime_uv "$root" \
    "$uv" venv --managed-python --no-project --no-config --python "$version" \
      "$stage" || {
        rm -rf "$stage"
        return 1
      }
  python="$(oms_python_runtime_env_python "$stage" 2>/dev/null || true)"
  if [ -z "$python" ] || ! oms_python_runtime_python_matches "$python" "$version"; then
    rm -rf "$stage"
    oms_python_runtime_fail "uv did not create the locked Python $version environment" || return
  fi
  {
    printf 'schema=1\n'
    printf 'version=%s\n' "$version"
  } > "$stage/.oh-my-setting-python-env"
  mv "$stage" "$env_dir"
  current_stage="$root/current.tmp.$$"
  printf '%s\n' "$version" > "$current_stage"
  mv "$current_stage" "$root/current"
  oms_python_runtime_write_launcher "$root" "$version" python3
  oms_python_runtime_write_launcher "$root" "$version" python
  oms_python_runtime_write_launcher "$root" "$version" python3 bin
}

oms_python_runtime_ensure() {
  local root
  root="$(oms_python_runtime_root)"
  mkdir -p "$(dirname "$root")" || return
  root="$(cd "$(dirname "$root")" && pwd -P)/$(basename "$root")" || return
  OMS_PYTHON_RUNTIME_ROOT="$root" oms_with_file_lock "$root/current" \
    oms_python_runtime_ensure_locked "$@"
}

# A fresh host may have neither python3 nor uv, while everything that reads
# tools.lock.json is Python. Fetch the pinned uv with curl, verify its sha256,
# and build the runtime from the lock without any interpreter. The uv lands
# where python-runtime.sh ensure keeps it, with the same owner record.
oms_python_runtime_bootstrap() {  # REPO_ROOT; needs platform.sh and file-lock.sh
  local lock="$1/tools.lock.json" platform uv_version url expected archive
  local version root uv_dir tmp name src stage
  platform="$(oms_tool_platform)" || return
  case "$platform" in windows-*) return 1 ;; esac
  uv_version="$(oms_lock_scalar "$lock" uv.version)" &&
    url="$(oms_lock_scalar "$lock" "uv.platforms.$platform.url")" &&
    expected="$(oms_lock_scalar "$lock" "uv.platforms.$platform.sha256")" &&
    archive="$(oms_lock_scalar "$lock" "uv.platforms.$platform.archive")" &&
    version="$(oms_lock_scalar "$lock" python.version)" ||
    oms_python_runtime_fail "cannot read uv and Python pins from $lock" || return
  [ "$archive" = tar.gz ] || oms_python_runtime_fail "unexpected uv archive: $archive" || return
  root="$(oms_python_runtime_root)"
  uv_dir="${OMS_PYTHON_RUNTIME_UV_BIN_DIR:-$(dirname "$root")/uv/bin}"
  if ! "$uv_dir/uv" --version 2>/dev/null | grep -Fq "uv $uv_version"; then
    command -v curl >/dev/null 2>&1 && command -v tar >/dev/null 2>&1 ||
      oms_python_runtime_fail "curl and tar are required to fetch uv" || return
    for name in uv uvx; do
      if [ -e "$uv_dir/$name" ] || [ -L "$uv_dir/$name" ]; then
        [ -f "$uv_dir/$name.oh-my-setting-managed" ] ||
          oms_python_runtime_fail "refusing to replace unowned $uv_dir/$name" || return
      fi
    done
    echo "fetching uv $uv_version ($platform) to build the OMS Python runtime"
    tmp="$(mktemp -d "${TMPDIR:-/tmp}/oms-uv.XXXXXX")" || return
    if ! curl --proto '=https' --tlsv1.2 -fsSL "$url" -o "$tmp/uv.tar.gz" ||
       [ "$(oms_sha256_file "$tmp/uv.tar.gz")" != "$expected" ] ||
       ! tar -xzf "$tmp/uv.tar.gz" -C "$tmp"; then
      rm -rf "$tmp"
      oms_python_runtime_fail "uv $uv_version download or sha256 verification failed" || return
    fi
    mkdir -p "$uv_dir"
    for name in uv uvx; do
      # Release archives hold uv-<target>/uv; minimal images may lack find.
      src=""
      for src in "$tmp"/*/"$name" "$tmp/$name"; do [ -f "$src" ] && break; done
      [ -f "$src" ] || { rm -rf "$tmp"; oms_python_runtime_fail "uv archive lacks $name" || return; }
      stage="$uv_dir/.$name.oh-my-setting-stage"
      cp "$src" "$stage" && chmod 0755 "$stage" && mv "$stage" "$uv_dir/$name" &&
        printf 'sha256=%s\n' "$(oms_sha256_file "$uv_dir/$name")" > "$uv_dir/$name.oh-my-setting-managed" ||
        { rm -rf "$tmp"; return 1; }
    done
    rm -rf "$tmp"
    "$uv_dir/uv" --version 2>/dev/null | grep -Fq "uv $uv_version" ||
      oms_python_runtime_fail "fetched uv does not report $uv_version" || return
  fi
  oms_python_runtime_ensure "$uv_dir/uv" "$version"
}

oms_python_runtime_activate() {
  local root python version node_version node_bin repo_root="${1:-${ROOT:-}}"
  root="$(oms_python_runtime_root)"
  python="$(oms_python_runtime_locked_python "$repo_root")" || {
    oms_python_runtime_fail "missing or invalid; reinstall or run doctor --repair"
    return 1
  }
  version="$(oms_python_runtime_locked_version "$repo_root")" || return
  [ -x "$root/launchers/$version/python3" ] && [ ! -L "$root/launchers" ] &&
    [ ! -L "$root/launchers/$version" ] || {
      oms_python_runtime_fail "missing runtime launcher; run python-runtime.sh ensure"
      return 1
    }
  unset PYTHONHOME PYTHONPATH
  node_version="$(oms_lock_scalar "$repo_root/tools.lock.json" node.version)" || return
  node_bin="${NVM_DIR:-$HOME/.nvm}/versions/node/v$node_version/bin"
  [ ! -x "$node_bin/node" ] || export PATH="$node_bin:$PATH"
  export OMS_PYTHON="$python"
  export OMS_PYTHON_RUNTIME_ACTIVE=1
  export PATH="$root/launchers/$version:$HOME/.local/bin:$PATH"
  hash -r 2>/dev/null || true
}

oms_python_runtime_activate_if_present() {
  oms_python_runtime_matches_lock "$ROOT" || return 0
  oms_python_runtime_activate "$ROOT"
}
