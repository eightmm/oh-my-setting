#!/usr/bin/env bash

# Shared, project-neutral Python runtime for OMS itself. Project environments
# remain untouched: activation only shadows python/python3 for the OMS process
# tree and deliberately does not set VIRTUAL_ENV or UV_PROJECT_ENVIRONMENT.

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

oms_python_runtime_current_version() {
  local root current version
  root="$(oms_python_runtime_root)"
  current="$root/current"
  [ -f "$current" ] && [ ! -L "$current" ] || return 1
  version="$(sed -n '1p' "$current" 2>/dev/null | tr -d '\r')"
  oms_python_runtime_version_valid "$version" || return 1
  printf '%s\n' "$version"
}

oms_python_runtime_current_python() {
  local root version env_dir python
  root="$(oms_python_runtime_root)"
  oms_python_runtime_root_owned "$root" && [ ! -L "$root/envs" ] || return 1
  version="$(oms_python_runtime_current_version)" || return 1
  env_dir="$root/envs/$version"
  oms_python_runtime_env_owned "$env_dir" "$version" || return 1
  python="$(oms_python_runtime_env_python "$env_dir")" || return 1
  oms_python_runtime_python_matches "$python" "$version" || return 1
  printf '%s\n' "$python"
}

oms_python_runtime_locked_version() {
  local repo_root="$1" python version
  [ -x "$repo_root/scripts/lib/tool-lock.py" ] && [ -f "$repo_root/tools.lock.json" ] ||
    return 1
  python="$(oms_python_runtime_current_python 2>/dev/null || command -v python3 2>/dev/null || true)"
  [ -n "$python" ] || return 1
  version="$("$python" -I "$repo_root/scripts/lib/tool-lock.py" \
    --lock "$repo_root/tools.lock.json" get python.version 2>/dev/null | tr -d '\r')" ||
    return 1
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

oms_python_runtime_write_launcher() {
  local root="$1" version="$2" name="$3" target stage
  target="$root/launchers/$version/$name"
  [ ! -L "$root/launchers/$version" ] || return 1
  mkdir -p "$root/launchers/$version"
  stage="$(mktemp "$root/launchers/$version/.$name.XXXXXX")" || return
  {
    printf '#!/usr/bin/env bash\nversion=%s\n' "$version"
    cat <<'EOF_LAUNCHER'
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
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
      return 0
    fi
    oms_python_runtime_fail "existing environment is invalid; preserved for repair: $env_dir" || return
  elif [ -e "$env_dir" ] || [ -L "$env_dir" ]; then
    oms_python_runtime_fail "refusing to replace an unowned environment: $env_dir" || return
  fi

  mkdir -p "$root/envs" "$root/managed"
  stage="$(mktemp -d "$root/envs/.stage.XXXXXX")" || return

  echo "installing OMS Python $version (private uv runtime)"
  UV_PYTHON_INSTALL_DIR="$root/managed" \
    "$uv" python install --install-dir "$root/managed" --no-bin --no-config \
      "$version" || { rmdir "$stage"; return 1; }
  UV_PYTHON_INSTALL_DIR="$root/managed" \
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
}

oms_python_runtime_ensure() {
  local root
  root="$(oms_python_runtime_root)"
  mkdir -p "$(dirname "$root")" || return
  root="$(cd "$(dirname "$root")" && pwd -P)/$(basename "$root")" || return
  OMS_PYTHON_RUNTIME_ROOT="$root" oms_with_file_lock "$root/current" \
    oms_python_runtime_ensure_locked "$@"
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
  node_version="$("$python" -I "$repo_root/scripts/lib/tool-lock.py" --lock "$repo_root/tools.lock.json" get node.version | tr -d '\r')" || return
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
