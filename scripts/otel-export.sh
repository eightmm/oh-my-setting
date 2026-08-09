#!/usr/bin/env bash
set -euo pipefail

# Export whitelisted OMS metadata to stdout or a local OTLP JSONL file; this command never sends network traffic.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"

command -v python3 >/dev/null 2>&1 || {
  echo "error: python3 is required" >&2
  exit 2
}
exec python3 "$ROOT/scripts/lib/otel-export.py" "$@"
