#!/usr/bin/env bash
set -euo pipefail

# Check the manifest, frontmatter, local references, and agent metadata before
# scripts/link.sh exposes the skill catalog to agents.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
command -v python3 >/dev/null 2>&1 || {
  echo "error: python3 is required to validate skills" >&2
  exit 1
}
python3 "$ROOT/scripts/validate-skills.py" "$ROOT"
echo "Custom skills live in $ROOT/custom-skills and are symlinked by scripts/link.sh."
