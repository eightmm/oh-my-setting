#!/usr/bin/env bash
set -euo pipefail

# Check a recorded patch from a trusted spec; host commands require explicit consent and are not sandboxed.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"

command -v python3 >/dev/null 2>&1 || {
  echo "error: python3 is required" >&2
  exit 2
}
exec python3 "$ROOT/scripts/lib/semantic-eval.py" "$@"
