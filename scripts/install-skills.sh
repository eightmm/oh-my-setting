#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/skills.manifest.json"

if [ ! -f "$MANIFEST" ]; then
  echo "error: missing $MANIFEST" >&2
  exit 1
fi

echo "External skills are tracked in:"
echo "$MANIFEST"
echo
echo "Install curated skills with the target agent's skill installer."
echo "Keep custom skills in $ROOT/custom-skills."
