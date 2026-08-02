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

workflow="$ROOT/.github/workflows/test.yml"
for host in ubuntu-latest macos-latest windows-latest; do
  grep -Fq "os: $host" "$workflow" ||
    fail "install lifecycle matrix must cover $host"
done
# Copy mode is the Windows ownership contract. Proving it only on the Windows
# runner means the slowest leg in the matrix is the first to report a broken
# marker or a lost backup, so a Linux leg forces the same path early.
grep -Fq 'link_mode: copy' "$workflow" ||
  fail "install lifecycle matrix must exercise copy mode outside Windows"
# The macOS job is the only stock Bash 3.2 parser and the only BSD userland in
# CI. Both catch a class nothing else does, and both have already shipped
# breakage, so neither may quietly disappear again.
grep -Fq 'portability_macos:' "$workflow" ||
  fail "macOS portability job must exist"
grep -Fq 'bsd-portability-smoke.sh' "$workflow" ||
  fail "macOS portability job must run the BSD userland fixtures"
grep -Fq 'OMS_BASH32_BIN=/bin/bash' "$workflow" ||
  fail "macOS portability job must parse with the stock Bash 3.2"
# check.sh exits at the first failing stage, so lint, focused suites, and each
# large smoke shard are independent jobs. That preserves failure evidence and
# lets GitHub run the four expensive shards on separate runners.
grep -Fq -- '--lint-only' "$workflow" ||
  fail "lint must run as its own job"
grep -Fq 'focused:' "$workflow" ||
  fail "focused suites must run as their own job"
grep -Fq -- '--focused-only' "$workflow" ||
  fail "the focused job must use the focused-only gate mode"
grep -Fq 'smoke_shard:' "$workflow" ||
  fail "scripts-smoke must use a native matrix job"
grep -Fq 'shard: [1, 2, 3, 4]' "$workflow" ||
  fail "scripts-smoke matrix must retain four deterministic shards"
grep -Fq -- '--scripts-smoke-only' "$workflow" ||
  fail "each matrix child must run only its assigned scripts-smoke shard"
grep -Fq 'OMS_SMOKE_TIMINGS: "1"' "$workflow" ||
  fail "CI smoke shards must emit bounded timing evidence"

# Matrix child names are unstable branch-protection targets. One fixed gate
# depends on every required job and fails closed even when a dependency is
# cancelled or skipped.
grep -Fq 'gate:' "$workflow" || fail "workflow must expose one stable gate job"
grep -Fq 'if: always()' "$workflow" || fail "gate must evaluate failed and cancelled needs"
grep -Fq 'needs: [lint, focused, smoke_shard, install_e2e, portability_macos]' "$workflow" ||
  fail "gate must depend on every cross-platform verification job"
for result in lint focused smoke_shard install_e2e portability_macos; do
  grep -Fq "needs.$result.result" "$workflow" ||
    fail "gate does not inspect $result result"
done

for mode in --focused-only --scripts-smoke-only --quick; do
  grep -Fq -- "$mode" "$ROOT/scripts/check.sh" ||
    fail "check.sh does not expose $mode"
done
# An env prefix would export into every test the suite runs; a flag cannot.
if grep -qE 'OMS_CHECK_(LINT|TESTS)=' "$workflow"; then
  fail "the gate split must be passed as a flag, not an inherited variable"
fi
if grep -Fq 'OH_MY_SETTING_UPGRADE_GH' "$ROOT/scripts/install-tools.sh"; then
  fail "test-only gh upgrade controls must not be part of the product interface"
fi

echo "source-distribution-smoke: ok"
