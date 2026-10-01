#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-doctor-model.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() { echo "FAIL: $*" >&2; exit 1; }

fixture="$TMP/install"
home="$TMP/home"
bin="$TMP/bin"
project="$TMP/project"
mkdir -p "$fixture/scripts/lib" "$fixture/rules" "$fixture/.agents/plugins" \
  "$fixture/plugins/oh-my-setting/.codex-plugin" "$fixture/plugins/oh-my-setting" \
  "$fixture/prompts" "$home/.codex/skills" "$home/.claude/skills" \
  "$home/.gemini/antigravity/skills" "$home/.local/bin" "$bin" "$project"
cp "$ROOT/scripts/doctor.sh" "$fixture/scripts/doctor.sh"
cp "$ROOT/scripts/lib/tool-lock.py" "$ROOT/scripts/lib/doctor-probe-memo.sh" \
  "$ROOT/scripts/lib/doctor-probe-memo.py" "$ROOT/scripts/lib/run-bounded.py" "$fixture/scripts/lib/"
cp "$ROOT/tools.lock.json" "$fixture/tools.lock.json"
chmod +x "$fixture/scripts/doctor.sh"

cat > "$fixture/scripts/lib/agent-memory-common.sh" <<'EOF_STUB'
agent_memory_file_has_sensitive_content() { return 1; }
oms_platform_is_windows() { return 1; }
EOF_STUB
cat > "$fixture/scripts/lib/harness-residue.sh" <<'EOF_STUB'
oms_harness_count_stale_worktrees() { printf '0\n'; }
oms_harness_lock_residue_count() { printf '0\n'; }
oms_harness_tmp_residue_count() { printf '0\n'; }
oms_harness_count_unindexed_artifacts() { printf '0\n'; }
EOF_STUB
cat > "$fixture/scripts/lib/install-contract.sh" <<'EOF_STUB'
oms_install_receipt_path() { printf '%s\n' "${OMS_INSTALL_RECEIPT:-$HOME/.config/oh-my-setting/install.json}"; }
oms_install_plugin_version() { printf '0.0.0\n'; }
oms_install_plugin_hash() { printf 'unknown\n'; }
oms_install_tree_hash() { printf 'unknown\n'; }
oms_install_target_matches() {
  [ -L "$2" ] && [ -e "$2" ] && [ "$(readlink "$2")" = "$1" ]
}
oms_install_target_mode() {
  if [ -L "$2" ] && [ "$(readlink "$2")" = "$1" ]; then
    printf 'symlink\n'
  else
    printf 'foreign\n'
  fi
}
EOF_STUB
cat > "$fixture/scripts/skill-doctor.sh" <<'EOF_STUB'
#!/usr/bin/env bash
echo 'skill-doctor: ok'
EOF_STUB
cat > "$fixture/scripts/install-skills.sh" <<'EOF_STUB'
#!/usr/bin/env bash
exit 0
EOF_STUB
cat > "$fixture/scripts/model-doctor.sh" <<'EOF_STUB'
#!/usr/bin/env bash
printf 'model-doctor-args:'
printf ' %s' "$@"
printf '\n'
if [ "${MODEL_DOCTOR_FAIL:-0}" = 1 ]; then
  echo 'installed CLI is missing required flags: --effort'
  exit 1
fi
exit 0
EOF_STUB
cat > "$fixture/scripts/oms" <<'EOF_STUB'
#!/usr/bin/env bash
exit 0
EOF_STUB
chmod +x "$fixture/scripts/skill-doctor.sh" "$fixture/scripts/install-skills.sh" \
  "$fixture/scripts/model-doctor.sh" "$fixture/scripts/oms"
printf '# rules\n' > "$fixture/rules/global-AGENTS.md"
printf '{"skills":[]}\n' > "$fixture/skills.manifest.json"
printf '{"name":"fixture"}\n' > "$fixture/.agents/plugins/marketplace.json"
printf '{"version":"0.0.0"}\n' > "$fixture/plugins/oh-my-setting/.codex-plugin/plugin.json"
printf '{}\n' > "$fixture/plugins/oh-my-setting/hooks.json"

ln -s "$fixture/rules/global-AGENTS.md" "$home/.codex/AGENTS.md"
ln -s "$fixture/rules/global-AGENTS.md" "$home/.claude/CLAUDE.md"
ln -s "$fixture/rules/global-AGENTS.md" "$home/.gemini/AGENTS.md"
ln -s "$fixture/prompts" "$home/.oh-my-setting-prompts"
ln -s "$fixture/scripts/oms" "$home/.local/bin/oms"
cat > "$bin/claude" <<'EOF_STUB'
#!/usr/bin/env bash
exit 0
EOF_STUB
chmod +x "$bin/claude"

# An inherited NVM_DIR would put the host's real node bin ahead of the stubs.
run_doctor() {
  (cd "$project" && env -u NVM_DIR HOME="$home" XDG_CONFIG_HOME="$home/.config" \
    PATH="$bin:/usr/bin:/bin" OH_MY_SETTING_REQUIRE_TOOLS=0 \
    OH_MY_SETTING_CODEX_PLUGIN=0 "$fixture/scripts/doctor.sh" "$@")
}

out="$TMP/compatible.out"
run_doctor > "$out"
grep -Fq '# model capabilities' "$out" || fail "doctor did not run model capability section"
grep -Fq 'model-doctor-args:' "$out" || fail "doctor did not invoke model-doctor"
grep -Fq 'doctor: ok' "$out" || fail "successful model capability check should pass doctor"

warn="$TMP/warn.out"
MODEL_DOCTOR_FAIL=1 run_doctor > "$warn"
grep -Fq 'installed CLI is missing required flags: --effort' "$warn" ||
  fail "automatic model check did not surface CLI contract drift"
grep -Fq 'warn: model capability check failed' "$warn" ||
  fail "automatic model failure should remain a visible warning"
grep -Fq 'doctor: ok' "$warn" || fail "automatic model check should not block recovery"

forced="$TMP/forced.out"
rc=0
MODEL_DOCTOR_FAIL=1 OH_MY_SETTING_MODEL_DOCTOR=1 run_doctor > "$forced" 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "enforced model check should fail doctor, got $rc"
grep -Fq 'doctor: failed' "$forced" || fail "enforced doctor failure summary absent"

skip="$TMP/skip.out"
MODEL_DOCTOR_FAIL=1 run_doctor --no-model-doctor > "$skip"
if grep -Fq '# model capabilities' "$skip"; then
  fail "--no-model-doctor did not disable the capability check"
fi
grep -Fq 'doctor: ok' "$skip" || fail "model-doctor escape hatch should preserve recovery"

strict="$TMP/strict.out"
rc=0
MODEL_DOCTOR_FAIL=1 run_doctor --strict-diversity > "$strict" 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "strict diversity should enforce model-doctor failure"
grep -Fq 'model-doctor-args: --strict-diversity' "$strict" ||
  fail "doctor did not forward strict diversity"

live="$TMP/live.out"
rc=0
MODEL_DOCTOR_FAIL=1 run_doctor --live-models > "$live" 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "live model validation should enforce model-doctor failure"
grep -Fq 'model-doctor-args: --live-models' "$live" ||
  fail "doctor did not forward live model validation"

# A broken remote-tracking ref must not look like a healthy legacy install.
git -C "$fixture" init -q
git -C "$fixture" -c user.name=fixture -c user.email=fixture@example.com \
  -c commit.gpgsign=false commit -q --allow-empty -m fixture
run_doctor --no-model-doctor > "$TMP/git-healthy.out"
grep -Fq 'ok: install Git HEAD and references' "$TMP/git-healthy.out" ||
  fail 'doctor did not check install Git refs'
mkdir -p "$fixture/.git/refs/remotes/origin"
printf 'ATOM fixture data, not a Git reference\n' > "$fixture/.git/refs/remotes/origin/main"
cp "$fixture/.git/refs/remotes/origin/main" "$TMP/damaged-ref"
rc=0
run_doctor --no-model-doctor > "$TMP/git-damaged.out" 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "damaged install refs should fail doctor, got $rc"
grep -Fq 'install Git HEAD or references are invalid' "$TMP/git-damaged.out" ||
  fail 'doctor did not identify damaged install Git refs'
cmp "$TMP/damaged-ref" "$fixture/.git/refs/remotes/origin/main" ||
  fail 'doctor modified the damaged reference'

# Use the real model-doctor here; the stub above verifies failure policy.
rm -f "$fixture/.git/refs/remotes/origin/main"
cp "$ROOT/scripts/model-doctor.sh" "$fixture/scripts/model-doctor.sh"
for source in provider-registry.sh model-capability.sh; do
  cp "$ROOT/scripts/lib/$source" "$fixture/scripts/lib/$source"
done
cp "$ROOT/scripts/lib/doctor-probe-memo.py" "$fixture/scripts/lib/doctor-probe-memo.actual.py"
cp "$ROOT/scripts/lib/doctor-report.py" "$fixture/scripts/lib/doctor-report.py"
mv "$fixture/scripts/lib/tool-lock.py" "$fixture/scripts/lib/tool-lock.actual.py"
cat > "$fixture/scripts/lib/tool-lock.py" <<'EOF_STUB'
#!/usr/bin/env python3
import json
import os
import runpy
import sys
if "get" in sys.argv[1:]:
    with open(os.environ["TEST_PROBE_LOG"], "a", encoding="utf-8") as log:
        log.write("tool-lock.py " + json.dumps(sys.argv[1:]) + "\n")
runpy.run_path(os.path.join(os.path.dirname(__file__), "tool-lock.actual.py"), run_name="__main__")
EOF_STUB
cat > "$fixture/scripts/lib/doctor-probe-memo.py" <<'EOF_STUB'
#!/usr/bin/env python3
import os
import sys
if os.environ.get("TEST_UNCACHED") == "1":
    os.environ.pop("OMS_DOCTOR_PROBE_DIR", None)
os.execv(sys.executable, [sys.executable,
    os.path.join(os.path.dirname(__file__), "doctor-probe-memo.actual.py"), *sys.argv[1:]])
EOF_STUB
chmod +x "$fixture/scripts/lib/tool-lock.py" "$fixture/scripts/model-doctor.sh"

for tool in claude codex cursor-agent; do
  cat > "$bin/$tool" <<'EOF_STUB'
#!/usr/bin/env bash
tool="${0##*/}"
{ printf '%s' "$tool"; printf ' <%s>' "$@"; printf '\n'; } >> "$TEST_PROBE_LOG"
case " $* " in
  *' --version '*) printf '%s 1.2.3\n' "$tool" ;;
  *) printf 'Usage: %s --help\n' "$tool" ;;
esac
EOF_STUB
  chmod +x "$bin/$tool"
done
cat > "$bin/npm" <<'EOF_STUB'
#!/usr/bin/env bash
{ printf npm; printf ' <%s>' "$@"; printf '\n'; } >> "$TEST_PROBE_LOG"
case " $* " in
  *' list '*)
    python3 - "$TEST_TOOL_LOCK" "$5" <<'PY'
import json
import sys
lock = json.load(open(sys.argv[1], encoding="utf-8"))
package = sys.argv[2]
version = next((item["version"] for item in lock["npm"].values()
                if item["package"] == package), "")
print(json.dumps({"dependencies": {package: {"version": version}}}))
PY
    ;;
  *' prefix '*) printf '%s/.npm-global\n' "$HOME" ;;
  *' root '*) printf '%s/.npm-global/lib/node_modules\n' "$HOME" ;;
  *) printf '10.0.0\n' ;;
esac
EOF_STUB
chmod +x "$bin/npm"
python3 - "$fixture/tools.lock.json" "$home/.npm-global/lib/node_modules" <<'PY'
import json
from pathlib import Path
import sys
lock = json.load(open(sys.argv[1], encoding="utf-8"))
root = Path(sys.argv[2])
for item in lock["npm"].values():
    package = root / item["package"]
    package.mkdir(parents=True, exist_ok=True)
    (package / "package.json").write_text(
        json.dumps({"name": item["package"], "version": item["version"]}), encoding="utf-8")
    for native in item.get("native", {}).values():
        payload = package / "node_modules" / native["alias"]
        payload.mkdir(parents=True, exist_ok=True)
        (payload / "package.json").write_text(
            json.dumps({"name": native["package"], "version": native["version"]}), encoding="utf-8")
PY

run_logged_doctor() { # LOG OUTPUT ERROR RC_FILE
  local log="$1" output="$2" error="$3" rc_file="$4" rc=0
  shift 4
  : > "$log"
  TEST_PROBE_LOG="$log" TEST_TOOL_LOCK="$fixture/tools.lock.json" \
    TMPDIR="$TMP" run_doctor "$@" > "$output" 2> "$error" || rc=$?
  printf '%s\n' "$rc" > "$rc_file"
}
assert_probes_once() { # LOG
  [ -s "$1" ] || fail 'doctor ran no logged external probes'
  awk 'seen[$0]++ { print "duplicate probe: " $0 > "/dev/stderr"; bad=1 }
    END { exit bad }' "$1" || fail 'a doctor probe ran more than once in one run'
  for probe in 'claude <--version>' 'codex <--version>' \
    'cursor-agent <--version>' 'npm <prefix> <-g>' 'npm <root> <-g>'; do
    grep -Fxq "$probe" "$1" || fail "doctor did not exercise $probe"
  done
  grep -Fq 'tool-lock.py ' "$1" || fail 'doctor did not exercise tool-lock get'
}

TEST_UNCACHED=0
run_logged_doctor "$TMP/probes-1.log" "$TMP/probes-1.out" "$TMP/probes-1.err" "$TMP/probes-1.rc"
assert_probes_once "$TMP/probes-1.log"
run_logged_doctor "$TMP/probes-2.log" "$TMP/probes-2.out" "$TMP/probes-2.err" "$TMP/probes-2.rc"
assert_probes_once "$TMP/probes-2.log"
cmp "$TMP/probes-1.log" "$TMP/probes-2.log" ||
  fail 'a second doctor run did not probe the same commands anew'

TEST_UNCACHED=1 run_logged_doctor "$TMP/probes-uncached.log" \
  "$TMP/probes-uncached.out" "$TMP/probes-uncached.err" "$TMP/probes-uncached.rc"
[ "$(grep -Fxc 'codex <--version>' "$TMP/probes-uncached.log")" -gt 1 ] ||
  fail 'uncached control did not disable memoization'
cmp "$TMP/probes-1.out" "$TMP/probes-uncached.out" || fail 'cached stdout changed'
cmp "$TMP/probes-1.err" "$TMP/probes-uncached.err" || fail 'cached stderr changed'
cmp "$TMP/probes-1.rc" "$TMP/probes-uncached.rc" || fail 'cached exit status changed'
run_logged_doctor "$TMP/json-cached.log" "$TMP/json-cached.out" \
  "$TMP/json-cached.err" "$TMP/json-cached.rc" --json
assert_probes_once "$TMP/json-cached.log"
TEST_UNCACHED=1 run_logged_doctor "$TMP/json-uncached.log" \
  "$TMP/json-uncached.out" "$TMP/json-uncached.err" "$TMP/json-uncached.rc" --json
cmp "$TMP/json-cached.out" "$TMP/json-uncached.out" || fail 'cached JSON changed'
cmp "$TMP/json-cached.err" "$TMP/json-uncached.err" || fail 'cached JSON stderr changed'
cmp "$TMP/json-cached.rc" "$TMP/json-uncached.rc" || fail 'cached JSON exit status changed'
for residue in "$TMP"/oms-doctor-probe.*; do
  [ ! -e "$residue" ] || fail 'doctor left a probe cache after exit'
done
echo 'doctor-probe-once: ok'

echo 'doctor-model-capability-smoke: ok'
