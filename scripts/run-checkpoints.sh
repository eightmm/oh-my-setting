#!/usr/bin/env bash
set -euo pipefail

# Read-only audit of training-run checkpoint directories (oms run checkpoints).
# The logic lives in scripts/lib/run_checkpoints.py; this resolves the repo
# root and hands over. It never deletes or opens a checkpoint file.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT_LIB="$ROOT/scripts/lib"
# shellcheck source=scripts/lib/oms-common.sh
. "$ROOT_LIB/oms-common.sh"

command -v python3 >/dev/null 2>&1 || {
  echo "run-checkpoints: python3 is required" >&2
  exit 2
}

repo="$(oms_repo_root "$PWD" 2>/dev/null || pwd -P)"
exec python3 "$ROOT_LIB/run_checkpoints.py" --repo "$repo" "$@"
