#!/usr/bin/env bash
set -euo pipefail

# Ask the OpenAI Decisions API structured questions.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
command -v python3 >/dev/null 2>&1 || { echo 'error: python3 is required' >&2; exit 2; }
export PYTHONDONTWRITEBYTECODE=1
exec python3 "$ROOT/scripts/lib/openai_decisions.py" "$@"
