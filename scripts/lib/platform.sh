#!/usr/bin/env bash
# shellcheck shell=bash

# Small platform boundary shared by installers and lifecycle scripts. Windows
# means a POSIX shell supplied by Git for Windows/MSYS2/Cygwin; WSL reports
# Linux and follows the normal Linux path.

oms_platform_name() {
  local value="${OMS_PLATFORM_OVERRIDE:-}"

  if [ -z "$value" ]; then
    value="$(uname -s 2>/dev/null || printf unknown)"
  fi
  case "$value" in
    windows|Windows|WINDOWS|MINGW*|MSYS*|CYGWIN*) printf 'windows\n' ;;
    macos|macOS|MacOS|Darwin) printf 'macos\n' ;;
    linux|Linux) printf 'linux\n' ;;
    *) printf 'unknown\n' ;;
  esac
}

oms_platform_is_windows() {
  [ "$(oms_platform_name)" = windows ]
}

# The managed `python3` shim, recognized by its exact shape. Lives on the
# platform boundary because both the installer that writes it and the
# uninstaller that removes it must agree on what "ours" means — anything else
# at that path is a user's launcher and is never touched. The uv variant
# resolves the interpreter at call time, so the shim survives uv relocating
# or upgrading its managed CPython.
oms_install_python_shim_owned() {
  local target="$1"
  local command

  [ -f "$target" ] && [ ! -L "$target" ] || return 1
  [ "$(sed -n '1p' "$target")" = '#!/usr/bin/env bash' ] || return 1
  [ "$(sed -n '2p' "$target")" = '# managed by oh-my-setting' ] || return 1
  command="$(sed -n '3p' "$target")"
  case "$command" in
    'exec python "$@"'|'exec py -3 "$@"'|'exec "$(uv python find)" "$@"') ;;
    *) return 1 ;;
  esac
  [ -z "$(sed -n '4p' "$target")" ]
}

oms_install_link_mode() {
  local mode="${OH_MY_SETTING_LINK_MODE:-auto}"

  case "$mode" in
    auto)
      if oms_platform_is_windows; then
        printf 'copy\n'
      else
        printf 'symlink\n'
      fi
      ;;
    copy|symlink)
      printf '%s\n' "$mode"
      ;;
    *)
      echo "error: OH_MY_SETTING_LINK_MODE must be auto, copy, or symlink" >&2
      return 2
      ;;
  esac
}
