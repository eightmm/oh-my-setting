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
    'exec "'*'/bin/python3" "$@"') ;;  # the OMS runtime, on a host without Python
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

# The locked tool archive for this host, e.g. linux-amd64.
oms_tool_platform() {
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

oms_sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | awk '{print $1}'
  elif command -v openssl >/dev/null 2>&1; then openssl dgst -sha256 "$1" | awk '{print $NF}'
  else echo "error: sha256sum, shasum, or openssl is required" >&2; return 1
  fi
}

# One scalar from tools.lock.json by dotted path, for the bootstrap that runs
# before any Python exists. It walks strings and braces, not lines, so it
# does not depend on formatting; a test compares it with tool-lock.py.
oms_lock_scalar() {  # oms_lock_scalar LOCK DOTTED.PATH
  awk -v want="$2" '
    { text = text $0 "\n" }
    END {
      # Escaped JSON strings need decoding that this bootstrap reader cannot do.
      if (index(text, "\\") != 0) exit 1
      n = length(text); depth = 0; lists = 0; key = ""; value = 0
      for (i = 1; i <= n; i++) {
        c = substr(text, i, 1)
        if (c == "\"") {
          s = ""
          for (i++; i <= n && (d = substr(text, i, 1)) != "\""; i++) {
            if (d == "\\") { i++; d = substr(text, i, 1) }
            s = s d
          }
          if (lists) continue
          if (value) {
            if (path(key) == want) { print s; found = 1; exit }
            value = 0
          } else key = s
        } else if (c == ":") value = 1
        else if (c == "{") { name[++depth] = value ? key : ""; value = 0 }
        else if (c == "}") depth--
        else if (c == "[") lists++
        else if (c == "]") lists--
        else if (c == ",") value = 0
        else if (value && !lists && c ~ /[-0-9tfn]/) {
          s = c
          while (i < n && index(",}] \t\r\n", substr(text, i + 1, 1)) == 0) s = s substr(text, ++i, 1)
          if (path(key) == want) { print s; found = 1; exit }
          value = 0
        }
      }
    }
    function path(leaf,   p, k) {
      p = ""
      for (k = 2; k <= depth; k++) p = p name[k] "."
      return p leaf
    }
    END { exit found ? 0 : 1 }
  ' "$1"
}
