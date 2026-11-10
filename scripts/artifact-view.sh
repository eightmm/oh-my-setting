#!/usr/bin/env bash
set -euo pipefail

# Build an offline experiment comparison and decision report from recorded results.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
# shellcheck source=scripts/lib/python-runtime.sh
. "$ROOT/scripts/lib/python-runtime.sh"
oms_python_runtime_require || exit 1
exec python3 "$ROOT/scripts/lib/artifact_view.py" "$@"
