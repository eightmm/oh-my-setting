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
chmod +x "$fixture/scripts/doctor.sh"

cat > "$fixture/scripts/lib/agent-memory-common.sh" <<'EOF_STUB'
agent_memory_file_has_sensitive_content() { return 1; }
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

run_doctor() {
  (cd "$project" && HOME="$home" XDG_CONFIG_HOME="$home/.config" \
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

echo 'doctor-model-capability-smoke: ok'
