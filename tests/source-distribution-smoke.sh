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

# The public floor is Python 3.9, so syntax and the parser-less Codex HUD path
# need a real 3.9 interpreter in CI rather than only a modern-parser promise.
grep -Fq 'python39:' "$workflow" || fail "Python 3.9 compatibility job must exist"
grep -Fq 'python-version: "3.9"' "$workflow" || fail "CI must install Python 3.9 explicitly"
grep -Fq 'codex-hud-config-smoke.sh' "$workflow" ||
  fail "Python 3.9 job must exercise the Codex HUD fallback"

# Matrix child names are unstable branch-protection targets. One fixed gate
# depends on every required job and fails closed even when a dependency is
# cancelled or skipped.
grep -Fq 'gate:' "$workflow" || fail "workflow must expose one stable gate job"
grep -Fq 'if: always()' "$workflow" || fail "gate must evaluate failed and cancelled needs"
grep -Fq 'needs: [lint, focused, smoke_shard, install_e2e, portability_macos, python39]' "$workflow" ||
  fail "gate must depend on every cross-platform verification job"
for result in lint focused smoke_shard install_e2e portability_macos python39; do
  grep -Fq "needs.$result.result" "$workflow" ||
    fail "gate does not inspect $result result"
done

for mode in --focused-only --scripts-smoke-only --quick; do
  grep -Fq -- "$mode" "$ROOT/scripts/check.sh" ||
    fail "check.sh does not expose $mode"
done

# Standalone smoke suites belong to the focused gate. The full local gate
# composes that same block, while lint already covers tests/*.sh. Keep each
# suite in exactly one execution list so full mode cannot run it twice.
python3 - "$ROOT/scripts/check.sh" <<'PY' || fail "new focused smoke registration is incomplete or duplicated"
import pathlib
import sys

text = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
start = 'if [ "$RUN_FOCUSED" = 1 ]; then'
end = 'if [ "$RUN_SMOKE" = 1 ]; then'
if text.count(start) != 1 or text.count(end) != 1:
    raise SystemExit("focused/smoke gate boundaries changed")
focused = text.split(start, 1)[1].split(end, 1)[0]
expected = {
    "lifecycle-events": "lifecycle-events-smoke.sh",
    "supervisor": "supervisor-smoke.sh",
    "lifecycle-provider-integration": "lifecycle-provider-integration-smoke.sh",
    "install-lifecycle-lock": "install-lifecycle-lock-smoke.sh",
    "file-lock-boundary": "file-lock-boundary-smoke.sh",
    "harness-residue-boundary": "harness-residue-boundary-smoke.sh",
    "patch-land-approval": "patch-land-approval-smoke.sh",
    "tool-lock": "tool-lock-smoke.sh",
    "provider-permissions-mcp-boundary": "provider-permissions-mcp-boundary-smoke.sh",
    "execution-profile": "execution-profile-smoke.sh",
    "herdr-adapter": "herdr-adapter-smoke.sh",
    "operator-tools": "operator-tools-smoke.sh",
}
for stage, suite in expected.items():
    invocation = "stage %s bash tests/%s" % (stage, suite)
    if focused.count(invocation) != 1:
        raise SystemExit("focused gate must contain exactly once: %s" % invocation)
    if text.count("bash tests/%s" % suite) != 1:
        raise SystemExit("gate duplicates suite invocation: %s" % suite)
if "tests/*.sh" not in text:
    raise SystemExit("shellcheck lint no longer covers tests/*.sh")
PY

# An env prefix would export into every test the suite runs; a flag cannot.
if grep -qE 'OMS_CHECK_(LINT|TESTS)=' "$workflow"; then
  fail "the gate split must be passed as a flag, not an inherited variable"
fi
if grep -Fq 'OH_MY_SETTING_UPGRADE_GH' "$ROOT/scripts/install-tools.sh"; then
  fail "test-only gh upgrade controls must not be part of the product interface"
fi

# A working council is the installed product, so the initial installer has no
# partial-install mode that silently omits provider and service CLIs. Update's
# separate --no-tools flag remains valid: it means "do not refresh binaries on
# every daily source update", not "install an incomplete harness".
if grep -Fq -- '--no-tools' "$ROOT/install.sh"; then
  fail "initial install must not expose a no-tools escape hatch"
fi
grep -Fq '"$DEST/scripts/install-tools.sh"' "$ROOT/install.sh" ||
  fail "initial install must always invoke install-tools"
if grep -Fq -- '--no-tools' "$ROOT/README.md" || grep -Fq -- '--no-tools' "$ROOT/README.ko.md"; then
  fail "README still documents a partial initial install"
fi

# Exercise the environment escape hatch as behavior, but put network-facing
# commands behind failing stubs so a future regression cannot download or
# install anything during this source-contract probe.
install_probe="$(mktemp -d "${TMPDIR:-/tmp}/oms-required-tools.XXXXXX")"
trap 'rm -rf "$install_probe"' EXIT HUP INT TERM
mkdir -p "$install_probe/bin" "$install_probe/home"
for blocked in git curl; do
  cat > "$install_probe/bin/$blocked" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$0 $*" >> "$OMS_TEST_NETWORK_LOG"
exit 99
EOF
  chmod +x "$install_probe/bin/$blocked"
done
install_status=0
install_out="$(HOME="$install_probe/home" PATH="$install_probe/bin:/usr/bin:/bin" \
  OMS_TEST_NETWORK_LOG="$install_probe/network.log" \
  OH_MY_SETTING_DIR="$install_probe/checkout" OH_MY_SETTING_INSTALL_TOOLS=0 \
  bash "$ROOT/install.sh" 2>&1)" || install_status=$?
[ "$install_status" -eq 2 ] ||
  fail "OH_MY_SETTING_INSTALL_TOOLS=0 must be rejected as invalid configuration: $install_out"
printf '%s' "$install_out" | grep -Fq 'tool installation is required' ||
  fail "tool-install rejection did not explain the required contract"
[ ! -s "$install_probe/network.log" ] ||
  fail "tool-install rejection reached a network-facing command"

python3 - "$ROOT/skills.manifest.json" <<'PY' || fail "start phrase must deterministically trigger the spec interview"
import json, sys
rows = json.load(open(sys.argv[1], encoding="utf-8"))["skills"]
row = next(item for item in rows if item["name"] == "oms-spec-interview")
assert "start this project" in [str(value).casefold() for value in row.get("triggers", [])]
PY

for command in agent-call agent-run agent-executor peer-ask peer-review peer-delegate plan-run; do
  help="$(bash "$ROOT/scripts/$command.sh" --help 2>&1)" ||
    fail "$command --help failed"
  printf '%s' "$help" | grep -Fq 'xhigh' || fail "$command help omits xhigh effort"
  printf '%s' "$help" | grep -Fq 'max' || fail "$command help omits max effort"
  printf '%s' "$help" | grep -Fq 'ultra' || fail "$command help omits ultra effort"
done

if grep -Fq 'oms run-capsule' "$ROOT/scripts/oms-init.sh"; then
  fail "oms init still recommends the removed run-capsule front door"
fi
if grep -Fq 'tier follows the work' "$ROOT/scripts/agent-run.sh"; then
  fail "agent-run help still claims removed automatic model tiers"
fi
if grep -Fq -- '--effort' "$ROOT/custom-skills/oms-agent-harness/references/model-routing.md"; then
  fail "model routing skill still advertises the wrong public effort option"
fi
if grep -Eq 'Legacy tier|deep-tier|`oms check`' "$ROOT/docs/COMPONENTS.md"; then
  fail "components guide still describes removed model tiers or command names"
fi
if grep -Eq 'agent-consult|model tier below|deep planning/gates' "$ROOT/rules/global-AGENTS.md"; then
  fail "global agent rules still direct agents to removed commands or model tiers"
fi

echo "source-distribution-smoke: ok"
