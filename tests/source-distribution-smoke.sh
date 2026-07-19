#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

fail() {
  echo "source-distribution-smoke: $*" >&2
  exit 1
}

for path in \
  .github/workflows/release.yml \
  .github/workflows/agent-snapshot.yml \
  docs/RELEASE.md \
  scripts/gen-checksums.sh \
  tests/release-contract-smoke.sh; do
  [ ! -e "$ROOT/$path" ] || fail "obsolete GitHub Release surface remains: $path"
done

for file in README.md README.ko.md docs/COMPONENTS.md docs/MIGRATION-0.4.md \
    .github/workflows/test.yml scripts/check.sh; do
  if grep -Eiq 'docs/RELEASE|releases/(latest|download)|release-contract-smoke|gen-checksums|tag-driven release|tag 기반 릴리스' "$ROOT/$file"; then
    fail "obsolete Release reference remains: $file"
  fi
done

grep -Fq 'raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh' "$ROOT/README.md" ||
  fail "README must retain the main source installer"
grep -Fq 'INSTALLER_DEFAULT_REF="edge"' "$ROOT/install.sh" ||
  fail "source installer must retain the edge channel default"

echo "source-distribution-smoke: ok"
