#!/usr/bin/env bash
set -euo pipefail

# Emit a deterministic SHA256SUMS manifest for the release-relevant tracked
# files (installers, rules, scripts, skills, roles, templates, plugins, and
# metadata).
# Output is sorted by path so the same checkout always produces the same bytes,
# and is suitable for `sha256sum -c`. Run from anywhere inside the checkout.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

git rev-parse --git-dir >/dev/null 2>&1 || {
  echo "error: not a git checkout: $ROOT" >&2
  exit 2
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1"
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1"
  else
    echo "error: need sha256sum or shasum" >&2
    exit 2
  fi
}

# Tracked release files only — deterministic and excludes local/untracked state.
# LC_ALL=C keeps the sort stable across locales.
files="$(git ls-files \
  AGENTS.md \
  CLAUDE.md \
  '.agents/plugins/marketplace.json' \
  install.sh \
  scripts/oms \
  'scripts/*.sh' \
  'scripts/*.py' \
  'scripts/lib/*.sh' \
  'scripts/lib/*.py' \
  'custom-skills/**' \
  'roles/**' \
  'templates/**' \
  'plugins/**' \
  'prompts/**' \
  'workflows/**' \
  skills.manifest.json \
  VERSION | LC_ALL=C sort)"

[ -n "$files" ] || { echo "error: no release files found" >&2; exit 2; }

while IFS= read -r f; do
  [ -n "$f" ] || continue
  sha256_of "$f"
done <<EOF
$files
EOF
