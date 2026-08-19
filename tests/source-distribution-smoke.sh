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

for file in README.md README.ko.md docs/COMPONENTS.md docs/MIGRATION-0.4.md docs/MIGRATION-0.5.md \
    .github/workflows/test.yml scripts/check.sh; do
  # This guard exists for THIS repository's retired GitHub-Release surface;
  # a third-party tool pinned from its own releases page (CI's shellcheck
  # binary) is distribution hygiene, not a Release-surface regression, so
  # the download pattern is scoped to this repo's slug.
  if grep -Eiq 'docs/RELEASE|oh-my-setting/releases/(latest|download)|releases/latest|release-contract-smoke|gen-checksums|tag-driven release|tag 기반 릴리스' "$ROOT/$file"; then
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
# The requirement is semantic, not textual: every job the workflow defines is a
# verification job, so the gate must depend on all of them and read each one's
# result. Matching one rendered `needs:` line instead made the contract hostage
# to YAML formatting and to the job list of the day — adding a job passed the
# test while leaving the gate blind to it.
python3 - "$workflow" <<'PY' || fail "gate must depend on every verification job and inspect each result"
import pathlib
import re
import sys

lines = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()
try:
    start = next(i for i, line in enumerate(lines) if line.rstrip() == "jobs:")
except StopIteration:
    raise SystemExit("workflow defines no jobs block")

jobs = []
gate_body = []
current = None
for line in lines[start + 1:]:
    if line.strip() and not line.startswith(" "):
        break
    header = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", line)
    if header:
        current = header.group(1)
        jobs.append(current)
        continue
    if current == "gate":
        gate_body.append(line)

if "gate" not in jobs:
    raise SystemExit("workflow has no gate job")
required = set(jobs) - {"gate"}
if not required:
    raise SystemExit("workflow defines no verification jobs to gate on")

needs = set()
for index, line in enumerate(gate_body):
    inline = re.match(r"^\s{4}needs:\s*\[(.*)\]\s*$", line)
    if inline:
        needs = {name.strip() for name in inline.group(1).split(",") if name.strip()}
        break
    if re.match(r"^\s{4}needs:\s*$", line):
        for entry in gate_body[index + 1:]:
            item = re.match(r"^\s{6}-\s*([A-Za-z0-9_-]+)\s*$", entry)
            if not item:
                break
            needs.add(item.group(1))
        break

missing = sorted(required - needs)
if missing:
    raise SystemExit("gate does not depend on: %s" % ", ".join(missing))
unknown = sorted(needs - required)
if unknown:
    raise SystemExit("gate depends on jobs the workflow does not define: %s" % ", ".join(unknown))

body = "\n".join(gate_body)
unread = sorted(name for name in required if "needs.%s.result" % name not in body)
if unread:
    raise SystemExit("gate does not inspect the result of: %s" % ", ".join(unread))
PY

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
    "autopilot": "autopilot-smoke.sh",
    "draft-pr": "draft-pr-smoke.sh",
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
    "runtime-core": "runtime-core-smoke.sh",
    "runtime-core-integration": "runtime-core-integration-smoke.sh",
    "install-profile": "install-profile-smoke.sh",
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

# The installed product is capability-scoped: a fresh install provides the
# core runtime plus exactly one coding-agent provider and records a private
# capability receipt; --full remains the explicit compatibility footprint.
# The old all-tools contract had no partial mode; the new one has no silent
# one — a missing capability is reported unavailable, never faked as success,
# and there is still no escape hatch that skips tool installation entirely.
if grep -Fq -- '--no-tools' "$ROOT/install.sh"; then
  fail "initial install must not expose a no-tools escape hatch"
fi
grep -Fq 'CAPABILITY_PROFILES="${OH_MY_SETTING_CAPABILITY_PROFILES:-core}"' "$ROOT/install.sh" ||
  fail "fresh install must default to the core capability profile"
grep -Fq '"$DEST/scripts/install-profile.sh"' "$ROOT/install.sh" ||
  fail "capability installs must go through the selective installer"
grep -Fq '"$DEST/scripts/install-tools.sh"' "$ROOT/install.sh" ||
  fail "--full must keep the legacy all-tools path reachable"
grep -Fq 'CONNECT_SERVICES=0' "$ROOT/install.sh" ||
  fail "a non-full install must not run unselected service logins"
# Updating never silently shrinks an existing install: no capability receipt
# means the legacy full-tool refresh; a receipt updates only what was chosen.
grep -Fq -- '--reapply --upgrade' "$ROOT/scripts/update.sh" ||
  fail "update must reapply the recorded capability selection"
grep -Fq 'install-tools.sh" --upgrade' "$ROOT/scripts/update.sh" ||
  fail "receipt-less installs must keep the legacy full-tool update"
# The selective installer itself must enforce the capability semantics the
# runtime promises: one provider for core, Notion optional, council plural.
grep -Fq 'selected capabilities remain unavailable' "$ROOT/scripts/install-profile.sh" ||
  fail "a failed capability check must fail the apply, not masquerade as success"
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

# Shared-surface artifacts this repo ships carry the oms marker so their
# provenance is visible next to user-owned files in the same namespace
# (skills, output styles) and template installs stay collision-free.
# Repo-internal files referenced by path (roles/, prompts/, config/) and
# provider-mandated names are out of scope.
for surface in custom-skills templates/project-skills; do
  [ -d "$ROOT/$surface" ] || continue
  for entry in "$ROOT/$surface"/*/; do
    [ -d "$entry" ] || continue
    case "$(basename "$entry")" in
      oms-*) : ;;
      *) fail "$surface entry lacks the oms- prefix: $(basename "$entry")" ;;
    esac
  done
done
for entry in "$ROOT/output-styles"/*.md; do
  [ -f "$entry" ] || continue
  case "$(basename "$entry")" in
    oms-*) : ;;
    *) fail "output style lacks the oms- prefix: $(basename "$entry")" ;;
  esac
done
python3 - "$ROOT/skills.manifest.json" <<'PY' || fail "skills.manifest.json entries must keep the oms- prefix"
import json, os, sys
for row in json.load(open(sys.argv[1], encoding="utf-8"))["skills"]:
    assert row["name"].startswith("oms-"), row["name"]
    assert os.path.basename(row["source"]).startswith("oms-"), row["source"]
PY

echo "source-distribution-smoke: ok"
