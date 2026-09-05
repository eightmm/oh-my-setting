#!/usr/bin/env bash
set -euo pipefail

# Provision and enter the private uv-managed interpreter used by OMS itself.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
# shellcheck source=scripts/lib/python-runtime.sh
. "$ROOT/scripts/lib/python-runtime.sh"
# shellcheck source=scripts/lib/file-lock.sh
. "$ROOT/scripts/lib/file-lock.sh"

if [ "${OMS_HARNESS_CHILD:-0}" = 1 ] && [ "${1:-}" = ensure ]; then
  echo "error: a harness child cannot provision the OMS host runtime" >&2
  exit 2
fi

usage() {
  cat <<'EOF'
Usage: python-runtime.sh ensure|status|path|python|run COMMAND [ARGS...]

Manage the shared private Python used by OMS across projects. `ensure`
provisions the exact tools.lock.json version with the pinned uv bootstrap;
`run` executes a command with that interpreter first on PATH.
EOF
}

ensure_runtime() {
  local runtime_root uv_bin_dir uv_bin uv_version version
  unset PYTHONHOME PYTHONPATH
  command -v python3 >/dev/null 2>&1 || {
    echo "error: bootstrap python3 is required to validate tools.lock.json" >&2
    return 1
  }
  runtime_root="$(oms_python_runtime_root)"
  uv_bin_dir="${OMS_PYTHON_RUNTIME_UV_BIN_DIR:-$(dirname "$runtime_root")/uv/bin}"
  set --
  # shellcheck source=scripts/install-tools.sh
  . "$ROOT/scripts/install-tools.sh"
  uv_version="$(tool_lock_get uv.version)"
  uv_bin="$(command -v uv 2>/dev/null || true)"
  if [ -z "$uv_bin" ] || ! command_has_version "$uv_bin" "$uv_version"; then
    export OMS_UV_BIN_DIR="$uv_bin_dir"
    ensure_uv
    case "$(uname -s 2>/dev/null || true)" in
      MINGW*|MSYS*|CYGWIN*) uv_bin="$uv_bin_dir/uv.exe" ;;
      *) uv_bin="$uv_bin_dir/uv" ;;
    esac
  fi
  version="$(tool_lock_get python.version)"
  oms_python_runtime_ensure "$uv_bin" "$version"
  oms_python_runtime_activate
  echo "ok: OMS Python $version ($OMS_PYTHON)"
}

case "${1:-}" in
  ensure)
    [ "$#" -eq 1 ] || { usage >&2; exit 2; }
    ensure_runtime
    ;;
  status)
    [ "$#" -eq 1 ] || { usage >&2; exit 2; }
    if oms_python_runtime_matches_lock "$ROOT" &&
       python="$(oms_python_runtime_locked_python "$ROOT" 2>/dev/null)"; then
      echo "python-runtime: ok ($(oms_python_runtime_locked_version "$ROOT"), $python)"
    else
      echo "python-runtime: missing or invalid"
      exit 1
    fi
    ;;
  path)
    [ "$#" -eq 1 ] || { usage >&2; exit 2; }
    oms_python_runtime_matches_lock "$ROOT" || {
      echo "error: OMS Python runtime does not match tools.lock.json; run $0 ensure" >&2
      exit 1
    }
    printf '%s/launchers/%s\n' "$(oms_python_runtime_root)" "$(oms_python_runtime_locked_version "$ROOT")"
    ;;
  python)
    [ "$#" -eq 1 ] || { usage >&2; exit 2; }
    oms_python_runtime_matches_lock "$ROOT" || {
      echo "error: OMS Python runtime does not match tools.lock.json; run $0 ensure" >&2
      exit 1
    }
    oms_python_runtime_locked_python "$ROOT"
    ;;
  run)
    shift
    [ "$#" -gt 0 ] || { echo "error: run requires a command" >&2; exit 2; }
    oms_python_runtime_matches_lock "$ROOT" || {
      echo "error: OMS Python runtime does not match tools.lock.json; run $0 ensure" >&2
      exit 1
    }
    oms_python_runtime_activate
    exec "$@"
    ;;
  -h|--help|help)
    usage
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
