#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-v04-update.XXXXXX")"
trap '[ "${KEEP_TMP:-0}" = 1 ] || rm -rf "$TMP"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }

export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=oms-test GIT_AUTHOR_EMAIL=test@example.com
export GIT_COMMITTER_NAME=oms-test GIT_COMMITTER_EMAIL=test@example.com

make_agy_health_stub() {
  mkdir -p "$1/bin"
  cat > "$1/bin/agy" <<'EOF_AGY'
#!/usr/bin/env bash
[ "$*" = 'plugin list' ] || exit 73
status=enabled
[ "${OMS_TEST_HEALTH_FAIL:-0}" = 0 ] || status=disabled
printf 'Installed plugins:\n- oh-my-setting (%s, 0.7.0)\n' "$status"
printf '%s\n' '      Source:     antigravity' '      Components: 1 MCP server'
EOF_AGY
  chmod +x "$1/bin/agy"
}

test_harness_child_cannot_mutate_install() {
  local fixture="$TMP/child-update"
  local output rc label
  mkdir -p "$fixture/scripts/lib"
  cp "$ROOT/scripts/update.sh" "$fixture/scripts/update.sh"
  cp "$ROOT/scripts/lib/install-contract.sh" "$fixture/scripts/lib/install-contract.sh"
  cp "$ROOT/scripts/lib/install-lifecycle-lock.sh" \
    "$fixture/scripts/lib/install-lifecycle-lock.sh"
  cp "$ROOT/scripts/lib/file-lock.sh" "$fixture/scripts/lib/file-lock.sh"
  chmod +x "$fixture/scripts/update.sh"

  assert_child_update_refused() {
    label="$1"
    shift
    output="$TMP/child-update-$label.out"
    rc=0
    OMS_HARNESS_CHILD=1 "$fixture/scripts/update.sh" "$@" \
      >"$output" 2>&1 || rc=$?
    [ "$rc" -eq 2 ] || fail "child update $label returned $rc: $(cat "$output")"
    grep -Fq 'a harness child cannot update or probe the OMS installation' "$output" ||
      fail "child update $label refusal was not actionable: $(cat "$output")"
  }

  assert_child_update_refused default
  assert_child_update_refused check --check
  assert_child_update_refused rollback --rollback
  assert_child_update_refused probe --probe-agy-surfaces
  OMS_HARNESS_CHILD=1 "$fixture/scripts/update.sh" --help >/dev/null
}

test_schema1_receipt_migrates_to_profiled_schema2() {
  local home="$TMP/schema-home"
  local receipt="$home/.config/oh-my-setting/install.json"

  mkdir -p "$(dirname "$receipt")"
  python3 - "$receipt" "$ROOT" <<'PY'
import json, sys
json.dump({
    "schema": 1,
    "source_root": sys.argv[2],
    "commit": "0123456789abcdef0123456789abcdef01234567",
    "channel": "main",
    "version": "0.3.0",
    "installed_at": "2026-07-12T00:00:00Z",
    "plugin": {"name": "oh-my-setting", "version": "0.1.0", "sha256": "x" * 64},
}, open(sys.argv[1], "w", encoding="utf-8"))
PY
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_PROFILE=minimal OH_MY_SETTING_REF=edge \
    OH_MY_SETTING_CLAUDE_HOOKS=0 OH_MY_SETTING_CODEX_PLUGIN=0 \
    OH_MY_SETTING_AUTO_UPDATE=0 "$ROOT/scripts/link.sh" >/dev/null
  python3 - "$receipt" <<'PY' || fail "schema-1 receipt did not migrate"
import json, sys
d = json.load(open(sys.argv[1], encoding="utf-8"))
assert d["schema"] == 2
assert d["profile"] == "minimal"
assert d["ref"] == "edge"
assert d["components"]["claude_hooks"] is False
assert d["components"]["codex_plugin"] is False
assert d["components"]["auto_update"] is False
assert ".oh-my-setting-workflows" not in d["managed_targets"]
PY
}

test_update_rolls_back_and_supports_explicit_rollback() {
  local source="$TMP/source"
  local installed="$TMP/installed"
  local home="$TMP/update-home"
  local receipt="$home/.config/oh-my-setting/install.json"
  local first
  local failed
  local good
  local before_receipt="$TMP/receipt-before"
  local expected_status=0

  git clone -q "$ROOT" "$source"
  # Pull-request checkouts are detached. Give the fixture an explicit default
  # branch so update.sh can resolve edge through origin/HEAD.
  git -C "$source" checkout -qB main
  # During development the root may be dirty; copy the transaction files so
  # this fixture always exercises the working implementation.
  cp "$ROOT/scripts/update.sh" "$source/scripts/update.sh"
  cp "$ROOT/scripts/link.sh" "$source/scripts/link.sh"
  cp "$ROOT/scripts/unlink.sh" "$source/scripts/unlink.sh"
  cp "$ROOT/scripts/doctor.sh" "$source/scripts/doctor.sh"
  cp "$ROOT/tools.lock.json" "$source/tools.lock.json"
  cp "$ROOT/scripts/lib/tool-lock.py" "$source/scripts/lib/tool-lock.py"
  # The doctor verifies the hook registration the installer writes, so the two
  # must come from the same tree — a working-tree doctor against a HEAD-clone
  # installer fails on any hook the working tree added.
  cp "$ROOT/scripts/install-claude-hooks.sh" "$source/scripts/install-claude-hooks.sh"
  cp "$ROOT/scripts/lib/claude-hook-surfaces.py" "$source/scripts/lib/claude-hook-surfaces.py"
  cp "$ROOT/scripts/claude-statusline.py" "$source/scripts/claude-statusline.py"
  cp "$ROOT/scripts/claude-subagent-statusline.py" "$source/scripts/claude-subagent-statusline.py"
  cp "$ROOT/scripts/telemetry-hook.sh" "$source/scripts/telemetry-hook.sh"
  cp "$ROOT/scripts/precompact-handoff.sh" "$source/scripts/precompact-handoff.sh"
  cp "$ROOT/scripts/resume-hook.sh" "$source/scripts/resume-hook.sh"
  cp "$ROOT/scripts/install-mcp.sh" "$source/scripts/install-mcp.sh"
  cp "$ROOT/scripts/install-agy-plugin.sh" "$source/scripts/install-agy-plugin.sh"
  cp "$ROOT/scripts/oms-mcp-server.py" "$source/scripts/oms-mcp-server.py"
  cp "$ROOT/scripts/lib/install-contract.sh" "$source/scripts/lib/install-contract.sh"
  cp "$ROOT/scripts/lib/install-lifecycle-lock.sh" "$source/scripts/lib/install-lifecycle-lock.sh"
  cp "$ROOT/scripts/lib/file-lock.sh" "$source/scripts/lib/file-lock.sh"
  cp "$ROOT/scripts/lib/poll.sh" "$source/scripts/lib/poll.sh"
  cp "$ROOT/scripts/lib/platform.sh" "$source/scripts/lib/platform.sh"
  cp "$ROOT/scripts/lib/managed-target.py" "$source/scripts/lib/managed-target.py"
  cp "$ROOT/scripts/lib/agent-install-state.sh" "$source/scripts/lib/agent-install-state.sh"
  rm -rf "$source/workflows"
  rm -f "$source/scripts/multi-agent-ask.sh" "$source/scripts/multi-agent-review.sh" \
    "$source/scripts/multi-agent-delegate.sh"
  git -C "$source" add -A
  git -C "$source" commit -qm "fixture: v0.4 base" || true
  first="$(git -C "$source" rev-parse HEAD)"

  git clone -q "$source" "$installed"
  mkdir -p "$home"
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_PROFILE=minimal OH_MY_SETTING_REF=edge \
    OH_MY_SETTING_CLAUDE_HOOKS=0 OH_MY_SETTING_CODEX_PLUGIN=0 \
    OH_MY_SETTING_MCP=0 OH_MY_SETTING_AGY_PLUGIN=0 \
    OH_MY_SETTING_AUTO_UPDATE=0 "$installed/scripts/link.sh" >/dev/null
  cp "$receipt" "$before_receipt"
  rm -f "$home/.codex/AGENTS.md"
  printf 'user rules\n' > "$home/.codex/AGENTS.md"
  printf 'older backup\n' > "$home/.codex/AGENTS.md.backup.20260701000000"
  # The Antigravity skill root is part of the snapshot too: relinking would
  # restore the missing link, so a rolled-back update must leave it missing.
  rm -f "$home/.gemini/config/skills/oms-agent-harness"
  printf 'user skill\n' > "$home/.gemini/config/skills/USER.txt"
  # The Claude output style is relinked by the update, so it is snapshotted too.
  rm -f "$home/.claude/output-styles/oms-korean.md"
  printf 'user style\n' > "$home/.claude/output-styles/oms-korean.md"

  printf '#!/usr/bin/env bash\nexit 41\n' > "$source/scripts/doctor.sh"
  chmod +x "$source/scripts/doctor.sh"
  git -C "$source" add scripts/doctor.sh
  git -C "$source" commit -qm "fixture: failing doctor"
  failed="$(git -C "$source" rev-parse HEAD)"

  if HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    PATH="/usr/bin:/bin" "$installed/scripts/update.sh" --no-tools >"$TMP/fail.out" 2>&1; then
    fail "doctor failure should fail the update"
  fi
  [ "$(git -C "$installed" rev-parse HEAD)" = "$first" ] ||
    fail "failed update did not restore the previous HEAD"
  cmp -s "$receipt" "$before_receipt" || fail "failed update changed the receipt"
  [ ! -L "$home/.claude/output-styles/oms-korean.md" ] &&
    grep -Fxq 'user style' "$home/.claude/output-styles/oms-korean.md" ||
    fail "failed update did not restore the Claude output style"
  grep -Fq "rollback restored $first" "$TMP/fail.out" || fail "rollback was not reported"
  [ ! -L "$home/.codex/AGENTS.md" ] && grep -Fxq 'user rules' "$home/.codex/AGENTS.md" ||
    fail "failed update did not restore the user-owned rules file"
  [ "$(find "$home/.codex" -maxdepth 1 -name 'AGENTS.md.backup.*' | wc -l | tr -d ' ')" = 1 ] ||
    fail "failed update changed the pre-existing backup set"
  grep -Fxq 'older backup' "$home/.codex/AGENTS.md.backup.20260701000000" ||
    fail "failed update changed the pre-existing backup"
  [ ! -e "$home/.gemini/config/skills/oms-agent-harness" ] &&
    [ ! -L "$home/.gemini/config/skills/oms-agent-harness" ] &&
    grep -Fxq 'user skill' "$home/.gemini/config/skills/USER.txt" ||
    fail "failed update did not restore the Antigravity skill root"
  [ "$failed" != "$first" ] || fail "fixture did not create a failing target"

  rm -f "$home/.codex/AGENTS.md" "$home/.codex/AGENTS.md.backup.20260701000000"
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_PROFILE=minimal OH_MY_SETTING_REF=edge \
    OH_MY_SETTING_CLAUDE_HOOKS=0 OH_MY_SETTING_CODEX_PLUGIN=0 \
    OH_MY_SETTING_MCP=0 OH_MY_SETTING_AGY_PLUGIN=0 \
    OH_MY_SETTING_AUTO_UPDATE=0 "$installed/scripts/link.sh" >/dev/null

  cp "$installed/scripts/doctor.sh" "$source/scripts/doctor.sh"
  git -C "$source" add scripts/doctor.sh
  git -C "$source" commit -qm "fixture: healthy doctor"
  good="$(git -C "$source" rev-parse HEAD)"
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    PATH="/usr/bin:/bin" "$installed/scripts/update.sh" --no-tools >"$TMP/good.out" 2>&1 ||
    { cat "$TMP/good.out" >&2; fail "healthy update should succeed"; }
  [ "$(git -C "$installed" rev-parse HEAD)" = "$good" ] || fail "successful update missed target"
  grep -Fq 'mcp: skipped (OH_MY_SETTING_MCP=0)' "$TMP/good.out" || fail "clean update lost MCP opt-out"
  grep -Fq 'agy-plugin: skipped (OH_MY_SETTING_AGY_PLUGIN=0)' "$TMP/good.out" || fail "clean update lost Antigravity opt-out"
  python3 - "$receipt" <<'PY_OPTOUT'
import json, sys
r = json.load(open(sys.argv[1]))
assert r["components"]["mcp"] is False and r["components"]["agy_plugin"] is False
assert r["component_modes"]["agy_plugin"] == "0"
PY_OPTOUT
  [ "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["previous_commit"])' "$receipt")" = "$first" ] ||
    fail "successful update did not record previous_commit"
  ! grep -Fq 'skill-router.sh' "$home/.claude/settings.json" 2>/dev/null ||
    fail "plain update re-enabled a disabled Claude hook"
  [ ! -e "$home/.config/systemd/user/oh-my-setting-autoupdate.timer" ] ||
    fail "plain update re-enabled a disabled update timer"

  cp "$receipt" "$TMP/stray-target-receipt"
  local stray_target_rc=0
  env -u OH_MY_SETTING_UPDATE_EXPECTED_TARGET HOME="$home" XDG_CONFIG_HOME="$home/.config" \
    OMS_INSTALL_RECEIPT="$receipt" OH_MY_SETTING_UPDATE_TARGET_COMMIT="$first" \
    PATH="/usr/bin:/bin" "$installed/scripts/update.sh" --no-tools > "$TMP/stray-target.out" 2>&1 ||
    stray_target_rc=$?
  [ "$stray_target_rc" = 75 ] ||
    fail "manual update accepted a stray selected-target override (exit $stray_target_rc)"
  [ "$(git -C "$installed" rev-parse HEAD)" = "$good" ] ||
    fail "stray selected-target override changed HEAD"
  cmp -s "$receipt" "$TMP/stray-target-receipt" ||
    fail "stray selected-target override changed the receipt"
  grep -Fq 'selected update target requires the expected-target fence' "$TMP/stray-target.out" ||
    fail "stray selected-target override refusal was not explained"

  cp "$receipt" "$TMP/expected-target-receipt"
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_UPDATE_EXPECTED_TARGET="$first" PATH="/usr/bin:/bin" \
    "$installed/scripts/update.sh" --no-tools >"$TMP/expected-target.out" 2>&1 ||
    expected_status=$?
  [ "$expected_status" = 75 ] ||
    fail "an update target changed after preflight should exit 75, got $expected_status"
  [ "$(git -C "$installed" rev-parse HEAD)" = "$good" ] ||
    fail "expected-target refusal changed HEAD"
  cmp -s "$receipt" "$TMP/expected-target-receipt" ||
    fail "expected-target refusal changed the receipt"
  grep -Fq 'update target changed after preflight' "$TMP/expected-target.out" ||
    fail "expected-target refusal was not explained"

  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    PATH="/usr/bin:/bin" "$installed/scripts/update.sh" --rollback --no-tools >/dev/null
  [ "$(git -C "$installed" rev-parse HEAD)" = "$first" ] || fail "explicit rollback missed previous commit"
  [ "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["previous_commit"])' "$receipt")" = "$good" ] ||
    fail "rollback did not retain the forward recovery point"

  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    PATH="/usr/bin:/bin" "$installed/scripts/update.sh" --ref edge --no-tools >/dev/null
  [ "$(git -C "$installed" rev-parse HEAD)" = "$good" ] || fail "edge switch missed remote HEAD"
  [ "$(git -C "$installed" symbolic-ref --short HEAD)" = main ] ||
    fail "edge switch did not restore the remote default branch"
  [ "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["ref"])' "$receipt")" = edge ] ||
    fail "edge switch was not persisted"

  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_CLAUDE_HOOKS=1 PATH="/usr/bin:/bin" \
    "$installed/scripts/update.sh" --no-tools >/dev/null
  grep -Fq 'skill-router.sh' "$home/.claude/settings.json" ||
    fail "explicit component override did not enable the Claude hook"
  [ "$(python3 -c 'import json,sys; print(str(json.load(open(sys.argv[1]))["components"]["claude_hooks"]).lower())' "$receipt")" = true ] ||
    fail "explicit component override was not persisted"

  printf 'dirty\n' > "$installed/local-change"
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    PATH="/usr/bin:/bin" "$installed/scripts/update.sh" --check >/dev/null ||
    fail "read-only update check should work on a dirty checkout"
  if HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    PATH="/usr/bin:/bin" "$installed/scripts/update.sh" --no-tools >/dev/null 2>&1; then
    fail "mutating update accepted a dirty checkout"
  fi
  [ "$(git -C "$installed" rev-parse HEAD)" = "$good" ] ||
    fail "dirty-check refusal changed HEAD"
  rm -f "$installed/local-change"

  git -C "$installed" checkout -qB main "$first"
  printf 'divergent\n' > "$installed/local-only"
  git -C "$installed" add local-only
  git -C "$installed" commit -qm "fixture: divergent local edge"
  git -C "$installed" checkout -q --detach "$first"
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_PROFILE=minimal OH_MY_SETTING_REF=edge \
    OH_MY_SETTING_CLAUDE_HOOKS=0 OH_MY_SETTING_CODEX_PLUGIN=0 \
    OH_MY_SETTING_MCP=0 OH_MY_SETTING_AGY_PLUGIN=0 \
    OH_MY_SETTING_AUTO_UPDATE=0 "$installed/scripts/link.sh" >/dev/null
  cp "$receipt" "$TMP/divergent-receipt"
  if HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    PATH="/usr/bin:/bin" "$installed/scripts/update.sh" --ref edge --no-tools \
    >/dev/null 2>&1; then
    fail "edge transition accepted a divergent local default branch"
  fi
  [ "$(git -C "$installed" rev-parse HEAD)" = "$first" ] ||
    fail "failed edge transition did not restore detached HEAD"
  if git -C "$installed" symbolic-ref -q HEAD >/dev/null; then
    fail "failed edge transition did not restore detached state"
  fi
  cmp -s "$receipt" "$TMP/divergent-receipt" ||
    fail "failed edge transition changed the receipt"
}

test_doctor_failure_restores_previous_plugin_payload() {
  local source="$TMP/plugin-source"
  local installed="$TMP/plugin-installed"
  local home="$TMP/plugin-home"
  local receipt="$home/.config/oh-my-setting/install.json"
  local bin="$TMP/plugin-bin"
  local state="$home/plugin-state"
  local codex_config="$home/.codex/config.toml"
  local first
  local claude_config="$home/.claude.json"
  local agy_payload="$home/.gemini/config/plugins/oh-my-setting"

  mkdir -p "$source/scripts/lib" "$bin" "$(dirname "$receipt")" "$(dirname "$codex_config")"
  cp "$ROOT/scripts/update.sh" "$source/scripts/update.sh"
  cp "$ROOT/scripts/lib/install-contract.sh" "$source/scripts/lib/install-contract.sh"
  cp "$ROOT/scripts/lib/install-lifecycle-lock.sh" "$source/scripts/lib/install-lifecycle-lock.sh"
  cp "$ROOT/scripts/lib/file-lock.sh" "$source/scripts/lib/file-lock.sh"
  cp "$ROOT/scripts/lib/poll.sh" "$source/scripts/lib/poll.sh"
  cp "$ROOT/scripts/lib/platform.sh" "$source/scripts/lib/platform.sh"
  cp "$ROOT/scripts/lib/managed-target.py" "$source/scripts/lib/managed-target.py"
  for script in link install-claude-hooks install-autoupdate uninstall-autoupdate install-tools install-mcp install-agy-plugin; do
    printf '#!/usr/bin/env bash\nexit 0\n' > "$source/scripts/$script.sh"
    chmod +x "$source/scripts/$script.sh"
  done
  cat > "$source/scripts/install-codex-plugin.sh" <<'EOF'
#!/usr/bin/env bash
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [ "${1:-}" = "--remove" ]; then rm -f "$OMS_PLUGIN_STATE"; else cp "$root/plugin-payload" "$OMS_PLUGIN_STATE"; fi
EOF
  printf '#!/usr/bin/env bash\nexit 0\n' > "$source/scripts/doctor.sh"
  printf 'old\n' > "$source/plugin-payload"
  chmod +x "$source/scripts/install-codex-plugin.sh" "$source/scripts/doctor.sh"
  git -C "$source" init -q
  git -C "$source" checkout -qb main
  git -C "$source" add .
  git -C "$source" commit -qm "fixture: old plugin"
  first="$(git -C "$source" rev-parse HEAD)"
  git clone -q "$source" "$installed"
  printf 'old\n' > "$state"
  printf '[tui]\nanimations = true\n' > "$codex_config"
  printf '#!/usr/bin/env bash\nexit 0\n' > "$bin/codex"
  chmod +x "$bin/codex"
  python3 - "$receipt" "$installed" "$first" <<'PY'
import json, sys
json.dump({
    "schema": 2, "source_root": sys.argv[2], "commit": sys.argv[3],
    "channel": "main", "dirty": False, "version": "0.4.0",
    "profile": "custom", "ref": "edge", "previous_commit": "",
    "installed_at": "2026-07-12T00:00:00Z",
    "components": {"tools": False, "claude_hooks": False, "codex_plugin": True,
                   "auto_update": False, "machine_snapshot": False, "slurm_snapshot": False},
    "managed_targets": [],
    "plugin": {"name": "oh-my-setting", "version": "old", "sha256": "x" * 64},
}, open(sys.argv[1], "w", encoding="utf-8"))
PY

  printf '{"user":"keep","mcpServers":{}}\n' > "$claude_config"
  cp "$claude_config" "$TMP/claude-before.json"
  mkdir -p "$agy_payload"
  printf 'old agy\n' > "$agy_payload/plugin.json"
  cat > "$source/scripts/install-mcp.sh" <<'EOF_MCP'
#!/usr/bin/env bash
printf '{"mcpServers":{"oh-my-setting":{"args":["new"]}}}\n' > "$HOME/.claude.json"
EOF_MCP
  cat > "$source/scripts/install-agy-plugin.sh" <<'EOF_AGY'
#!/usr/bin/env bash
payload="$HOME/.gemini/config/plugins/oh-my-setting"
mkdir -p "$payload"
printf 'new agy\n' > "$payload/plugin.json"
printf 'new hook\n' > "$payload/hooks.json"
EOF_AGY
  printf 'new\n' > "$source/plugin-payload"
  cat > "$source/scripts/install-codex-plugin.sh" <<'EOF'
#!/usr/bin/env bash
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
config="${CODEX_HOME:-$HOME/.codex}/config.toml"
if [ "${1:-}" = "--remove" ]; then
  rm -f "$OMS_PLUGIN_STATE"
else
  cp "$root/plugin-payload" "$OMS_PLUGIN_STATE"
  printf '[tui]\nstatus_line = ["model"]\n' > "$config"
  printf 'new backup\n' > "$config.oms-bak"
fi
EOF
  printf '#!/usr/bin/env bash\nexit 41\n' > "$source/scripts/doctor.sh"
  git -C "$source" add plugin-payload scripts
  git -C "$source" commit -qm "fixture: new plugin with failing doctor"

  if HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OMS_PLUGIN_STATE="$state" PATH="$bin:/usr/bin:/bin" \
    "$installed/scripts/update.sh" --no-tools >/dev/null 2>&1; then
    fail "plugin fixture update should fail doctor"
  fi
  grep -Fxq old "$state" || fail "rollback left the target plugin payload installed"
  grep -Fq 'animations = true' "$codex_config" ||
    fail "plugin rollback did not restore the user's Codex config"
  [ ! -e "$codex_config.oms-bak" ] ||
    fail "plugin rollback left a backup created by the failed target"
  [ "$(git -C "$installed" rev-parse HEAD)" = "$first" ] ||
    fail "plugin rollback did not restore source HEAD"
  cmp -s "$claude_config" "$TMP/claude-before.json" || fail "rollback lost Claude MCP/user config"
  grep -Fxq 'old agy' "$agy_payload/plugin.json" || fail "rollback left new Antigravity payload"
  [ ! -e "$agy_payload/hooks.json" ] || fail "rollback left newly generated Antigravity hooks"
  rm -f "$claude_config"
  rm -rf "$agy_payload"
  if HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OMS_PLUGIN_STATE="$state" PATH="$bin:/usr/bin:/bin" \
    "$installed/scripts/update.sh" --no-tools >"$TMP/absent-integration.out" 2>&1; then
    fail "absent integration fixture should fail doctor"
  fi
  [ ! -e "$claude_config" ] && [ ! -e "$agy_payload" ] || fail "rollback retained newly created integrations"

  # Provider refresh is outside source rollback: prove a later health failure
  # keeps the successful core update and cannot print unconditional success.
  make_agy_health_stub "$home"
  cat > "$source/scripts/doctor.sh" <<'EOF_HEALTH'
#!/usr/bin/env bash
if [ "${OMS_DOCTOR_INTEGRATIONS_ONLY:-0}" = 1 ]; then
  [ -f "$HOME/provider-refreshed" ] || exit 42
  root="$(cd "$(dirname "$0")/.." && pwd -P)"
  . "$root/scripts/lib/install-contract.sh"
  state="$(oms_install_agy_plugin_state "$HOME/bin/agy")" || exit 1
  state="$(oms_strip_cr "$state")"
  [ "$state" = enabled ] || exit 1
fi
EOF_HEALTH
  cat > "$source/scripts/install-tools.sh" <<'EOF_TOOLS'
#!/usr/bin/env bash
touch "$HOME/provider-refreshed"
EOF_TOOLS
  touch "$source/scripts/lib/provider-latest.py"
  git -C "$source" add scripts
  git -C "$source" commit -qm "fixture: post-refresh health"
  if HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OMS_PLUGIN_STATE="$state" OMS_TEST_HEALTH_FAIL=1 PATH="$bin:/usr/bin:/bin" \
    "$installed/scripts/update.sh" >"$TMP/provider-health.out" 2>&1; then
    fail "provider health failure reported update success"
  fi
  [ "$(git -C "$installed" rev-parse HEAD)" = "$(git -C "$source" rev-parse HEAD)" ] || fail "provider failure rolled back successful core update"
  grep -Fq 'core update succeeded; provider integration health failed' "$TMP/provider-health.out" || fail "provider failure did not separate core outcome"
  if grep -Fxq 'update: ok' "$TMP/provider-health.out"; then fail "unhealthy providers reported update: ok"; fi
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OMS_PLUGIN_STATE="$state" PATH="$bin:/usr/bin:/bin" \
    "$installed/scripts/update.sh" >"$TMP/provider-healthy.out" 2>&1 || fail "healthy provider refresh failed"
  grep -Fxq 'update: ok' "$TMP/provider-healthy.out" || fail "healthy refresh did not report success"
}

test_schema1_update_preserves_channel_pin_and_cron() {
  local source="$TMP/schema1-source"
  local installed="$TMP/schema1-installed"
  local detached="$TMP/schema1-detached"
  local edge_installed="$TMP/schema1-edge-installed"
  local home="$TMP/schema1-update-home"
  local detached_home="$TMP/schema1-detached-home"
  local edge_home="$TMP/schema1-edge-home"
  local receipt="$home/.config/oh-my-setting/install.json"
  local detached_receipt="$detached_home/.config/oh-my-setting/install.json"
  local edge_receipt="$edge_home/.config/oh-my-setting/install.json"
  local cron_file="$home/cron.txt"
  local base
  local release_head
  local bad_tip
  local divergent merge_target alternate

  git clone -q "$ROOT" "$source"
  for file in scripts/update.sh scripts/link.sh scripts/doctor.sh \
    scripts/install-claude-hooks.sh scripts/lib/claude-hook-surfaces.py \
    scripts/claude-statusline.py \
    scripts/claude-subagent-statusline.py scripts/telemetry-hook.sh \
    scripts/precompact-handoff.sh scripts/resume-hook.sh \
    scripts/install-mcp.sh scripts/install-agy-plugin.sh \
    scripts/oms-mcp-server.py \
    tools.lock.json scripts/lib/tool-lock.py \
    scripts/lib/install-contract.sh scripts/lib/install-lifecycle-lock.sh \
    scripts/lib/file-lock.sh scripts/lib/poll.sh scripts/lib/platform.sh \
    scripts/lib/managed-target.py scripts/lib/agent-install-state.sh; do
    cp "$ROOT/$file" "$source/$file"
  done
  rm -rf "$source/workflows"
  git -C "$source" add -A
  git -C "$source" commit -qm "fixture: 0.4 migration base" || true
  base="$(git -C "$source" rev-parse HEAD)"
  git -C "$source" checkout -qb release-line
  printf 'release update\n' > "$source/release-marker"
  git -C "$source" add release-marker
  git -C "$source" commit -qm "fixture: release branch update"
  release_head="$(git -C "$source" rev-parse HEAD)"

  git clone -q "$source" "$installed"
  git -C "$installed" checkout -qB release-line "$base"
  git -C "$installed" branch --set-upstream-to=origin/release-line release-line >/dev/null
  mkdir -p "$home"
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_CLAUDE_HOOKS=0 OH_MY_SETTING_CODEX_PLUGIN=0 \
    "$installed/scripts/link.sh" >/dev/null
  python3 - "$receipt" "$installed" "$base" release-line <<'PY'
import json, sys
json.dump({
    "schema": 1, "source_root": sys.argv[2], "commit": sys.argv[3],
    "channel": sys.argv[4], "dirty": False, "version": "0.3.0",
    "installed_at": "2026-07-12T00:00:00Z",
    "plugin": {"name": "oh-my-setting", "version": "old", "sha256": "x" * 64},
}, open(sys.argv[1], "w", encoding="utf-8"))
PY
  printf '%s\n%s\n%s\n' '# oh-my-setting autoupdate:begin' '0 3 * * * old' \
    '# oh-my-setting autoupdate:end' > "$cron_file"
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_AUTO_UPDATE_CRON_FILE="$cron_file" OH_MY_SETTING_AUTO_UPDATE_METHOD=cron \
    PATH="/usr/bin:/bin" \
    "$installed/scripts/update.sh" --no-tools >/dev/null
  [ "$(git -C "$installed" rev-parse HEAD)" = "$release_head" ] ||
    fail "schema-1 branch install moved away from its recorded channel"
  python3 - "$receipt" <<'PY' || fail "schema-1 branch receipt did not migrate"
import json, sys
d = json.load(open(sys.argv[1], encoding="utf-8"))
assert d["schema"] == 2
assert d["ref"] == "release-line"
assert d["components"]["auto_update"] is True
PY
  grep -Fq '# oh-my-setting autoupdate:begin' "$cron_file" ||
    fail "schema-1 cron trigger was removed during migration"

  # A fallback target stays within the configured branch ref and current
  # checkout ancestry; the next update must still advance to that branch tip.
  git -C "$source" checkout -q release-line
  printf 'cancelled tip\n' > "$source/cancelled-tip"
  git -C "$source" add cancelled-tip
  git -C "$source" commit -qm 'fixture: cancelled tip'
  bad_tip="$(git -C "$source" rev-parse HEAD)"
  git -C "$installed" checkout -q --detach "$base"
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_AUTO_UPDATE_CRON_FILE="$cron_file" OH_MY_SETTING_AUTO_UPDATE_METHOD=cron \
    OH_MY_SETTING_UPDATE_EXPECTED_TARGET="$bad_tip" \
    OH_MY_SETTING_UPDATE_TARGET_COMMIT="$release_head" PATH="/usr/bin:/bin" \
    "$installed/scripts/update.sh" --no-tools > "$TMP/selected-target.out" 2>&1 ||
    { cat "$TMP/selected-target.out" >&2; fail "selected green commit should update safely"; }
  [ "$(git -C "$installed" rev-parse HEAD)" = "$release_head" ] ||
    fail "selected green commit missed the fallback target"
  [ "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["ref"])' "$receipt")" = "release-line" ] ||
    fail "selected green commit changed the configured receipt ref"
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_AUTO_UPDATE_CRON_FILE="$cron_file" OH_MY_SETTING_AUTO_UPDATE_METHOD=cron \
    PATH="/usr/bin:/bin" "$installed/scripts/update.sh" --no-tools >/dev/null
  [ "$(git -C "$installed" rev-parse HEAD)" = "$bad_tip" ] ||
    fail "the next update did not follow the configured branch ref"
  [ "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["ref"])' "$receipt")" = "release-line" ] ||
    fail "the next update changed the configured receipt ref"

  # When origin's default branch has no local branch yet, selected green
  # commits must still be checked out exactly instead of starting at its tip.
  git -C "$source" symbolic-ref HEAD refs/heads/release-line
  git clone -q "$source" "$edge_installed"
  git -C "$edge_installed" checkout -q --detach "$base"
  git -C "$edge_installed" branch -D release-line >/dev/null
  git -C "$edge_installed" checkout -qb installed-base "$base"
  [ "$(git -C "$edge_installed" symbolic-ref --quiet --short refs/remotes/origin/HEAD)" = "origin/release-line" ] ||
    fail "edge fixture did not use release-line as origin's default"
  if git -C "$edge_installed" show-ref --verify --quiet refs/heads/release-line; then
    fail "edge fixture unexpectedly has a local default branch"
  fi
  mkdir -p "$edge_home"
  HOME="$edge_home" XDG_CONFIG_HOME="$edge_home/.config" OMS_INSTALL_RECEIPT="$edge_receipt" \
    OH_MY_SETTING_REF=edge OH_MY_SETTING_CLAUDE_HOOKS=0 OH_MY_SETTING_CODEX_PLUGIN=0 \
    "$edge_installed/scripts/link.sh" >/dev/null
  HOME="$edge_home" XDG_CONFIG_HOME="$edge_home/.config" OMS_INSTALL_RECEIPT="$edge_receipt" \
    OH_MY_SETTING_UPDATE_EXPECTED_TARGET="$bad_tip" \
    OH_MY_SETTING_UPDATE_TARGET_COMMIT="$release_head" PATH="/usr/bin:/bin" \
    "$edge_installed/scripts/update.sh" --no-tools > "$TMP/edge-selected-target.out" 2>&1 ||
    { cat "$TMP/edge-selected-target.out" >&2; fail "edge fallback should apply the selected green commit"; }
  [ "$(git -C "$edge_installed" rev-parse HEAD)" = "$release_head" ] ||
    fail "edge fallback ended at the unverified tip instead of the green commit"
  [ "$(git -C "$edge_installed" symbolic-ref --quiet --short HEAD)" = "release-line" ] ||
    fail "edge fallback did not create the configured default branch"
  [ "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["ref"])' "$edge_receipt")" = "edge" ] ||
    fail "edge fallback changed the configured receipt ref"

  # If an existing edge branch cannot move back to the selected green commit,
  # reject the apply and restore the branch and HEAD from before checkout.
  git -C "$edge_installed" checkout -q installed-base
  git -C "$edge_installed" branch -f release-line "$bad_tip"
  cp "$edge_receipt" "$TMP/edge-receipt-before-mismatch"
  local mismatch_rc=0
  HOME="$edge_home" XDG_CONFIG_HOME="$edge_home/.config" OMS_INSTALL_RECEIPT="$edge_receipt" \
    OH_MY_SETTING_UPDATE_EXPECTED_TARGET="$bad_tip" \
    OH_MY_SETTING_UPDATE_TARGET_COMMIT="$release_head" PATH="/usr/bin:/bin" \
    "$edge_installed/scripts/update.sh" --no-tools > "$TMP/edge-head-mismatch.out" 2>&1 ||
    mismatch_rc=$?
  [ "$mismatch_rc" = 75 ] || fail "edge HEAD mismatch returned $mismatch_rc, not 75"
  [ "$(git -C "$edge_installed" rev-parse HEAD)" = "$base" ] ||
    fail "edge HEAD mismatch did not restore the previous commit"
  [ "$(git -C "$edge_installed" symbolic-ref --quiet --short HEAD)" = "installed-base" ] ||
    fail "edge HEAD mismatch did not restore the previous branch"
  cmp -s "$edge_receipt" "$TMP/edge-receipt-before-mismatch" ||
    fail "edge HEAD mismatch changed the receipt before rejecting the target"
  grep -Fq 'applied update target does not match selected commit' "$TMP/edge-head-mismatch.out" ||
    fail "edge HEAD mismatch was not explained"

  git -C "$source" checkout -qb divergent-from-base "$base"
  printf 'divergent parent\n' > "$source/divergent-marker"
  git -C "$source" add divergent-marker
  git -C "$source" commit -qm 'fixture: divergent parent'
  divergent="$(git -C "$source" rev-parse HEAD)"
  git -C "$source" checkout -qb merge-ref "$bad_tip"
  git -C "$source" merge --no-ff -s ours -m 'fixture: second-parent target' \
    divergent-from-base >/dev/null
  merge_target="$(git -C "$source" rev-parse HEAD)"
  git -C "$source" checkout -q release-line
  git -C "$source" merge --ff-only merge-ref >/dev/null
  git -C "$installed" fetch origin >/dev/null

  # The selected commit is inside the fetched ref but does not contain the
  # installed second parent, so the updater must reject the non-fast-forward.
  git -C "$installed" checkout -q --detach "$divergent"
  local selected_rc=0
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_UPDATE_EXPECTED_TARGET="$merge_target" \
    OH_MY_SETTING_UPDATE_TARGET_COMMIT="$bad_tip" PATH="/usr/bin:/bin" \
    "$installed/scripts/update.sh" --no-tools > "$TMP/invalid-descendant.out" 2>&1 ||
    selected_rc=$?
  [ "$selected_rc" = 75 ] || fail "non-descendant selected commit returned $selected_rc, not 75"
  [ "$(git -C "$installed" rev-parse HEAD)" = "$divergent" ] ||
    fail "non-descendant selected commit changed HEAD"
  grep -Fq 'selected update target is not a valid fast-forward' "$TMP/invalid-descendant.out" ||
    fail "non-descendant selected commit was not explained"

  # A commit ahead of HEAD but outside the fetched configured ref also fails.
  git -C "$source" checkout -qb alternate-from-base "$base"
  printf 'alternate parent\n' > "$source/alternate-marker"
  git -C "$source" add alternate-marker
  git -C "$source" commit -qm 'fixture: alternate parent'
  alternate="$(git -C "$source" rev-parse HEAD)"
  git -C "$installed" fetch origin >/dev/null
  git -C "$installed" checkout -q --detach "$base"
  selected_rc=0
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_UPDATE_EXPECTED_TARGET="$merge_target" \
    OH_MY_SETTING_UPDATE_TARGET_COMMIT="$alternate" PATH="/usr/bin:/bin" \
    "$installed/scripts/update.sh" --no-tools > "$TMP/invalid-ref-ancestor.out" 2>&1 ||
    selected_rc=$?
  [ "$selected_rc" = 75 ] || fail "commit outside configured ref returned $selected_rc, not 75"
  [ "$(git -C "$installed" rev-parse HEAD)" = "$base" ] ||
    fail "commit outside configured ref changed HEAD"
  [ "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["ref"])' "$receipt")" = "release-line" ] ||
    fail "rejected selected commits changed the configured receipt ref"

  git clone -q "$source" "$detached"
  git -C "$detached" checkout -q --detach "$base"
  mkdir -p "$detached_home"
  HOME="$detached_home" XDG_CONFIG_HOME="$detached_home/.config" \
    OMS_INSTALL_RECEIPT="$detached_receipt" OH_MY_SETTING_CLAUDE_HOOKS=0 \
    OH_MY_SETTING_CODEX_PLUGIN=0 "$detached/scripts/link.sh" >/dev/null
  python3 - "$detached_receipt" "$detached" "$base" detached <<'PY'
import json, sys
json.dump({
    "schema": 1, "source_root": sys.argv[2], "commit": sys.argv[3],
    "channel": sys.argv[4], "dirty": False, "version": "0.3.0",
    "installed_at": "2026-07-12T00:00:00Z",
    "plugin": {"name": "oh-my-setting", "version": "old", "sha256": "x" * 64},
}, open(sys.argv[1], "w", encoding="utf-8"))
PY
  HOME="$detached_home" XDG_CONFIG_HOME="$detached_home/.config" \
    OMS_INSTALL_RECEIPT="$detached_receipt" PATH="/usr/bin:/bin" \
    "$detached/scripts/update.sh" --no-tools >/dev/null
  [ "$(git -C "$detached" rev-parse HEAD)" = "$base" ] ||
    fail "schema-1 detached install did not preserve its recorded commit"
  [ "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["ref"])' "$detached_receipt")" = "$base" ] ||
    fail "detached migration did not persist the recorded commit as its ref"
}

test_signal_during_doctor_rolls_back_transaction() {
  local source="$TMP/signal-source"
  local installed="$TMP/signal-installed"
  local home="$TMP/signal-home"
  local receipt="$home/.config/oh-my-setting/install.json"
  local before="$TMP/signal-receipt-before"
  local first
  local rc=0

  mkdir -p "$source/scripts/lib" "$(dirname "$receipt")" "$home/.codex"
  cp "$ROOT/scripts/update.sh" "$source/scripts/update.sh"
  cp "$ROOT/scripts/lib/install-contract.sh" "$source/scripts/lib/install-contract.sh"
  cp "$ROOT/scripts/lib/install-lifecycle-lock.sh" "$source/scripts/lib/install-lifecycle-lock.sh"
  cp "$ROOT/scripts/lib/file-lock.sh" "$source/scripts/lib/file-lock.sh"
  cp "$ROOT/scripts/lib/poll.sh" "$source/scripts/lib/poll.sh"
  cat > "$source/scripts/link.sh" <<'EOF'
#!/usr/bin/env bash
mkdir -p "$HOME/.codex"
rm -f "$HOME/.codex/AGENTS.md"
ln -s /target/rules "$HOME/.codex/AGENTS.md"
EOF
  for script in install-claude-hooks install-codex-plugin install-autoupdate uninstall-autoupdate install-tools install-mcp install-agy-plugin; do
    printf '#!/usr/bin/env bash\nexit 0\n' > "$source/scripts/$script.sh"
    chmod +x "$source/scripts/$script.sh"
  done
  printf '#!/usr/bin/env bash\nexit 0\n' > "$source/scripts/doctor.sh"
  chmod +x "$source/scripts/update.sh" "$source/scripts/link.sh" "$source/scripts/doctor.sh"
  git -C "$source" init -q
  git -C "$source" checkout -qb main
  git -C "$source" add .
  git -C "$source" commit -qm "fixture: signal base"
  first="$(git -C "$source" rev-parse HEAD)"
  git clone -q "$source" "$installed"

  printf '#!/usr/bin/env bash\nkill -TERM "$PPID"\nexit 0\n' > "$source/scripts/doctor.sh"
  git -C "$source" add scripts/doctor.sh
  git -C "$source" commit -qm "fixture: signal doctor"
  printf 'user rules\n' > "$home/.codex/AGENTS.md"
  printf 'old backup\n' > "$home/.codex/AGENTS.md.backup.20260701000000"
  python3 - "$receipt" "$installed" "$first" <<'PY'
import json, sys
json.dump({
    "schema": 2, "source_root": sys.argv[2], "commit": sys.argv[3],
    "channel": "main", "dirty": False, "version": "0.4.0",
    "profile": "minimal", "ref": "edge", "previous_commit": "",
    "installed_at": "2026-07-12T00:00:00Z",
    "components": {"tools": False, "claude_hooks": False, "codex_plugin": False,
                   "auto_update": False, "machine_snapshot": False, "slurm_snapshot": False},
    "managed_targets": [],
    "plugin": {"name": "oh-my-setting", "version": "0.4.0", "sha256": "x" * 64},
}, open(sys.argv[1], "w", encoding="utf-8"))
PY
  cp "$receipt" "$before"

  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    PATH="/usr/bin:/bin" "$installed/scripts/update.sh" --no-tools \
    >"$TMP/signal.out" 2>&1 || rc=$?
  [ "$rc" = 143 ] || fail "TERM during doctor should exit 143, got $rc"
  [ "$(git -C "$installed" rev-parse HEAD)" = "$first" ] ||
    fail "signal rollback did not restore HEAD"
  cmp -s "$receipt" "$before" || fail "signal rollback changed the receipt"
  [ ! -L "$home/.codex/AGENTS.md" ] && grep -Fxq 'user rules' "$home/.codex/AGENTS.md" ||
    fail "signal rollback did not restore user rules"
  [ "$(find "$home/.codex" -maxdepth 1 -name 'AGENTS.md.backup.*' | wc -l | tr -d ' ')" = 1 ] ||
    fail "signal rollback changed backup files"
}

test_detached_schema2_auto_update_check() {
  local repo="$TMP/detached auto"
  local home="$TMP/detached-auto-home"
  local receipt="$home/.config/oh-my-setting/install.json"
  local commit

  mkdir -p "$repo/scripts/lib" "$repo/local" "$home/.config/oh-my-setting"
  cp "$ROOT/scripts/auto-update.sh" "$repo/scripts/auto-update.sh"
  printf 'import os; print("1 " + os.environ.get("OMS_TEST_CI_RESULT", "success"))\n' > "$repo/scripts/lib/ci-query.py"
  cp "$ROOT/scripts/lib/file-lock.sh" "$repo/scripts/lib/file-lock.sh"
  cp "$ROOT/scripts/lib/poll.sh" "$repo/scripts/lib/poll.sh"
  cp "$ROOT/scripts/lib/install-contract.sh" "$repo/scripts/lib/install-contract.sh"
  cp "$ROOT/scripts/lib/platform.sh" "$repo/scripts/lib/platform.sh"
  cp "$ROOT/scripts/lib/managed-target.py" "$repo/scripts/lib/managed-target.py"
  make_agy_health_stub "$home"
  cat > "$repo/scripts/update.sh" <<'EOF'
#!/usr/bin/env bash
if [ "${OMS_TEST_REFRESH_ORIGIN_HEAD:-0}" = 1 ] && [ "${1:-}" = --check ]; then
  git -C "$(dirname "$0")/.." symbolic-ref refs/remotes/origin/HEAD refs/remotes/origin/release
fi
commit="$(git -C "$(dirname "$0")/.." rev-parse HEAD)"
echo "current: ${commit:0:7}"
echo "update-check: up_to_date $commit"
EOF
  cat > "$repo/scripts/install-tools.sh" <<'EOF'
#!/usr/bin/env bash
[ "$1" = --providers ] || exit 2
printf 'provider refresh\n' >> "$HOME/provider-refresh.log"
exit "${OMS_TEST_PROVIDER_REFRESH_EXIT:-0}"
EOF
  cat > "$repo/scripts/doctor.sh" <<'EOF_HEALTH'
#!/usr/bin/env bash
[ "${OMS_DOCTOR_INTEGRATIONS_ONLY:-0}" = 1 ] || exit 2
[ -f "$HOME/provider-refresh.log" ] || exit 3
root="$(cd "$(dirname "$0")/.." && pwd -P)"
. "$root/scripts/lib/install-contract.sh"
state="$(oms_install_agy_plugin_state "$HOME/bin/agy")" || exit 1
state="$(oms_strip_cr "$state")"
[ "$state" = enabled ]
EOF_HEALTH
  chmod +x "$repo/scripts/install-tools.sh"
  printf 'local/\n' > "$repo/.gitignore"
  chmod +x "$repo/scripts/auto-update.sh" "$repo/scripts/update.sh"
  git -C "$repo" init -q
  git -C "$repo" checkout -qb main
  git -C "$repo" add .
  git -C "$repo" commit -qm "fixture: detached auto update"
  commit="$(git -C "$repo" rev-parse HEAD)"
  git -C "$repo" checkout -q --detach "$commit"
  python3 - "$receipt" "$repo" "$commit" <<'PY'
import json, sys
json.dump({"schema": 2, "source_root": sys.argv[2], "commit": sys.argv[3],
           "channel": "detached", "profile": "custom", "ref": "main",
           "components": {}, "managed_targets": [], "plugin": {}},
          open(sys.argv[1], "w"))
PY
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    "$repo/scripts/auto-update.sh" check > "$TMP/detached-auto.out"
  grep -Fq 'auto-update: up_to_date' "$TMP/detached-auto.out" ||
    fail "detached schema-2 check did not use its receipt ref"
  grep -Fq 'upstream: ref:main' "$TMP/detached-auto.out" ||
    fail "detached schema-2 check lost its receipt ref"
  if git -C "$repo" symbolic-ref --quiet refs/remotes/origin/HEAD >/dev/null 2>&1; then
    fail "edge label fixture unexpectedly has an origin default branch before check"
  fi
  python3 - "$receipt" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as stream:
    receipt = json.load(stream)
receipt["ref"] = "edge"
with open(sys.argv[1], "w", encoding="utf-8") as stream:
    json.dump(receipt, stream)
PY
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OMS_TEST_REFRESH_ORIGIN_HEAD=1 "$repo/scripts/auto-update.sh" check > "$TMP/edge-auto.out"
  grep -Fq 'upstream: release (edge: default branch)' "$TMP/edge-auto.out" ||
    fail "edge schema-2 check did not report the refreshed origin default branch"
  if grep -Fq 'detached HEAD; auto-update skipped' "$repo/local/auto-update.status"; then
    fail "detached schema-2 check was skipped"
  fi
  [ ! -e "$home/provider-refresh.log" ] || fail "check mode refreshed providers"
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    "$repo/scripts/auto-update.sh" apply >/dev/null
  [ "$(wc -l < "$home/provider-refresh.log" | tr -d ' ')" = 1 ] ||
    fail "unchanged OMS must still refresh providers exactly once"
  if HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OMS_TEST_HEALTH_FAIL=1 "$repo/scripts/auto-update.sh" apply >/dev/null; then
    fail "unchanged-source provider health failure reported success"
  fi
  grep -Fq 'core unchanged; provider integration health failed' "$repo/local/auto-update.status" ||
    fail "provider health failure not recorded"
  if HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OMS_TEST_PROVIDER_REFRESH_EXIT=1 "$repo/scripts/auto-update.sh" apply >/dev/null; then
    fail "provider refresh failure must not report success"
  fi
  grep -Fq 'status=failed' "$repo/local/auto-update.status" || fail "provider failure not recorded"
}

test_schema1_auto_update_reuses_update_transaction() {
  local repo="$TMP/schema1 auto transaction"
  local home="$TMP/schema1-auto-home"
  local receipt="$home/.config/oh-my-setting/install.json"
  local marker="$home/update.argv"
  local state="$home/auto-update.status"
  local base green_parent target counter

  mkdir -p "$repo/scripts/lib" "$repo/local" "$(dirname "$receipt")"
  cp "$ROOT/scripts/auto-update.sh" "$repo/scripts/auto-update.sh"
  cat > "$repo/scripts/lib/ci-query.py" <<'PY'
import os, sys
sha = sys.argv[2]
result = os.environ.get("OMS_TEST_CI_RESULT", "success")
if result == "eventual-success" and sha == os.environ["OMS_TEST_UPDATE_TARGET"]:
    if sys.argv[3] != "2400":
        raise SystemExit("auto-update did not pass the bounded CI wait budget")
    counter = os.environ["OMS_TEST_CI_COUNTER"]
    try:
        count = int(open(counter).read())
    except (OSError, ValueError):
        count = 0
    open(counter, "w").write(str(count + 1))
    # The stub models ci-query's bounded wait and reports the final result.
    result = "success"
elif sha == os.environ.get("OMS_TEST_CI_GREEN_SHA"):
    result = "success"
print("1 " + result)
PY
  cp "$ROOT/scripts/lib/file-lock.sh" "$repo/scripts/lib/file-lock.sh"
  cp "$ROOT/scripts/lib/poll.sh" "$repo/scripts/lib/poll.sh"
  cat > "$repo/scripts/update.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
printf '%s\n' "$*" >> "$OMS_TEST_UPDATE_MARKER"
current="$(git -C "$root" rev-parse HEAD)"
if [ "${1:-}" = --check ]; then
  printf 'current: %s\n' "${current:0:7}"
  printf 'update-check: available %s -> %s\n' "$current" "$OMS_TEST_UPDATE_TARGET"
  exit 0
fi
selected="${OH_MY_SETTING_UPDATE_TARGET_COMMIT:-$OMS_TEST_UPDATE_TARGET}"
[ "${OH_MY_SETTING_UPDATE_EXPECTED_TARGET:-}" = "$OMS_TEST_UPDATE_TARGET" ] || exit 75
[ "$selected" = "$OMS_TEST_EXPECTED_TARGET" ] || exit 75
printf 'target-commit=%s\n' "${OH_MY_SETTING_UPDATE_TARGET_COMMIT:-}" >> "$OMS_TEST_UPDATE_MARKER"
git -C "$root" checkout -q --detach "$selected"
printf 'updated: %s -> %s\n' "${current:0:7}" "${selected:0:7}"
EOF
  chmod +x "$repo/scripts/auto-update.sh" "$repo/scripts/update.sh"
  git -C "$repo" init -q
  git -C "$repo" checkout -qb main
  git -C "$repo" config user.name test
  git -C "$repo" config user.email test@example.com
  printf 'base\n' > "$repo/value"
  git -C "$repo" add .
  git -C "$repo" commit -qm base
  base="$(git -C "$repo" rev-parse HEAD)"
  printf 'green parent\n' >> "$repo/value"
  git -C "$repo" commit -qam 'green parent'
  green_parent="$(git -C "$repo" rev-parse HEAD)"
  printf 'target\n' > "$repo/value"
  git -C "$repo" commit -qam target
  target="$(git -C "$repo" rev-parse HEAD)"
  git -C "$repo" checkout -q --detach "$base"

  python3 - "$receipt" "$repo" "$base" <<'PY'
import json, sys
json.dump({
    "schema": 1,
    "source_root": sys.argv[2],
    "commit": sys.argv[3],
    "channel": "main",
    "components": {},
}, open(sys.argv[1], "w", encoding="utf-8"))
PY

  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_AUTO_UPDATE_STATE="$state" OH_MY_SETTING_AUTO_UPDATE_LOG="$home/auto-update.log" \
    OMS_TEST_UPDATE_MARKER="$marker" \
    OMS_TEST_UPDATE_TARGET="$target" "$repo/scripts/auto-update.sh" check >/dev/null
  grep -Fxq -- '--check' "$marker" ||
    fail "schema-1 auto-update check bypassed the canonical updater"

  counter="$home/ci-counter"
  local ci_result expected_target inherited_target
  for ci_result in eventual-success cancelled failed no-green; do
    : > "$marker"
    rm -f "$counter"
    expected_target="$target"
    case "$ci_result" in
      cancelled|no-green) OMS_TEST_CI_RESULT=cancelled ;;
      failed) OMS_TEST_CI_RESULT=failure ;;
      *) OMS_TEST_CI_RESULT=eventual-success ;;
    esac
    if [ "$ci_result" = cancelled ]; then expected_target="$green_parent"; fi
    if [ "$ci_result" = no-green ]; then OMS_TEST_CI_RESULT=cancelled; fi
    HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
      OH_MY_SETTING_AUTO_UPDATE_STATE="$state" OH_MY_SETTING_AUTO_UPDATE_LOG="$home/auto-update.log" \
      OMS_TEST_CI_RESULT="$OMS_TEST_CI_RESULT" OMS_TEST_CI_COUNTER="$counter" \
      OMS_TEST_CI_GREEN_SHA="$([ "$ci_result" = cancelled ] && printf '%s' "$green_parent")" \
      OMS_TEST_UPDATE_MARKER="$marker" OMS_TEST_UPDATE_TARGET="$target" \
      OMS_TEST_EXPECTED_TARGET="$expected_target" \
      "$repo/scripts/auto-update.sh" apply >/dev/null
    if [ "$ci_result" = eventual-success ] || [ "$ci_result" = cancelled ]; then
      [ "$(git -C "$repo" rev-parse HEAD)" = "$expected_target" ] ||
        fail "CI $ci_result did not apply the expected target"
      grep -Fq 'status=applied' "$state" || fail "CI $ci_result apply outcome missing"
      if [ "$ci_result" = cancelled ]; then
        grep -Fq -- "target-commit=$expected_target" "$marker" ||
          fail "CI $ci_result did not pass the selected commit through the internal target contract"
        if grep -Fq -- '--ref' "$marker"; then
          fail "CI $ci_result changed the configured install ref with a CLI override"
        fi
        python3 - "$receipt" <<'PY'
import json, sys
assert json.load(open(sys.argv[1], encoding="utf-8"))["channel"] == "main"
PY
      fi
      [ "$(grep -c '^--check$' "$marker")" = 1 ] ||
        fail "schema-1 CI $ci_result did not preflight exactly once"
      if grep -Fxq -- '--no-tools' "$marker"; then
        fail "schema-1 auto-update must not suppress provider updates"
      fi
      git -C "$repo" checkout -q --detach "$base"
    else
      [ "$(git -C "$repo" rev-parse HEAD)" = "$base" ] || fail "unconfirmed CI moved schema-1 checkout"
      [ "$(cat "$marker")" = --check ] || fail "unconfirmed CI invoked the mutating updater"
      grep -Fq 'CI success not confirmed' "$state" || fail "CI deferral reason missing"
      if [ "$ci_result" = failed ]; then
        grep -Fq 'failed' "$state" || fail "failed CI reason missing"
      else
        grep -Fq 'no green descendant' "$state" || fail "no green descendant reason missing"
      fi
    fi
  done

  # Caller-provided internal overrides must not defeat this run's own CI
  # selection, whether the stale value names an intermediate or current HEAD.
  for inherited_target in "$green_parent" "$base"; do
    git -C "$repo" checkout -q --detach "$base"
    : > "$marker"
    HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
      OH_MY_SETTING_AUTO_UPDATE_STATE="$state" OH_MY_SETTING_AUTO_UPDATE_LOG="$home/auto-update.log" \
      OMS_TEST_CI_RESULT=success OMS_TEST_UPDATE_MARKER="$marker" \
      OMS_TEST_UPDATE_TARGET="$target" OMS_TEST_EXPECTED_TARGET="$target" \
      OH_MY_SETTING_UPDATE_TARGET_COMMIT="$inherited_target" \
      "$repo/scripts/auto-update.sh" apply > /dev/null
    [ "$(git -C "$repo" rev-parse HEAD)" = "$target" ] ||
      fail "inherited target override $inherited_target defeated the green tip"
    grep -Fq -- "target-commit=$target" "$marker" ||
      fail "auto-update did not pass its selected green tip over an inherited override"
  done
  git -C "$repo" checkout -q --detach "$base"

  : > "$marker"
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_AUTO_UPDATE_STATE="$state" OH_MY_SETTING_AUTO_UPDATE_LOG="$home/auto-update.log" \
    OMS_TEST_CI_RESULT=success OMS_TEST_UPDATE_MARKER="$marker" \
    OMS_TEST_UPDATE_TARGET="$target" OMS_TEST_EXPECTED_TARGET="$target" \
    "$repo/scripts/auto-update.sh" apply >/dev/null
  [ "$(git -C "$repo" rev-parse HEAD)" = "$target" ] ||
    fail "the next scheduled run did not follow the configured ref after fallback"
  grep -Fxq "target-commit=$target" "$marker" ||
    fail "the next scheduled run did not replace the fallback target with the green tip"
  python3 - "$receipt" <<'PY'
import json, sys
assert json.load(open(sys.argv[1], encoding="utf-8"))["channel"] == "main"
PY
}

test_ci_query_waits_for_pending_run() {
  local gh="$TMP/gh-ci-query" counter="$TMP/gh-ci-query-count" sha result elapsed
  sha=0123456789abcdef0123456789abcdef01234567
  cat > "$gh" <<'PY'
#!/usr/bin/env python3
import json, os, sys
import time
args = sys.argv[1:]
sha = args[args.index("--commit") + 1]
counter = os.environ["OMS_TEST_CI_COUNTER"]
try:
    count = int(open(counter).read())
except (OSError, ValueError):
    count = 0
open(counter, "w").write(str(count + 1))
delay = os.environ.get("OMS_TEST_CI_DELAY")
if delay:
    time.sleep(float(delay))
    status, conclusion = "completed", "success"
elif count > 0 and os.environ.get("OMS_TEST_CI_SLOW_SECOND"):
    time.sleep(float(os.environ["OMS_TEST_CI_SLOW_SECOND"]))
    status, conclusion = "completed", "success"
else:
    status = "in_progress" if count == 0 else "completed"
    conclusion = None if count == 0 else "success"
print(json.dumps([{"databaseId": 42, "status": status,
                   "conclusion": conclusion, "headSha": sha}]))
PY
  chmod +x "$gh"
  result="$(OMS_GH_BIN="$gh" OMS_TEST_CI_COUNTER="$counter" \
    python3 "$ROOT/scripts/lib/ci-query.py" \
    https://github.com/example/repo.git "$sha" 11 "" test.yml)" ||
    fail "ci-query rejected a pending run that completed within its budget"
  [ "$result" = '42 success' ] || fail "ci-query did not return the completed success: $result"
  [ "$(cat "$counter")" = 2 ] || fail "ci-query did not poll the pending run"
  rm -f "$counter"
  elapsed="$(OMS_GH_BIN="$gh" OMS_TEST_CI_COUNTER="$counter" OMS_TEST_CI_SLOW_SECOND=1.2 \
    python3 - "$ROOT/scripts/lib/ci-query.py" "$sha" <<'PY'
import subprocess, sys, time
start = time.monotonic()
run = subprocess.run([sys.executable, sys.argv[1], "https://github.com/example/repo.git",
                      sys.argv[2], "11", "", "test.yml"],
                     stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
elapsed = time.monotonic() - start
if run.returncode or run.stdout.strip() != "42 pending" or elapsed > 11.8:
    print("unexpected positive-budget result: rc=%s output=%r elapsed=%.3f" %
          (run.returncode, run.stdout.strip(), elapsed), file=sys.stderr)
    raise SystemExit(1)
print("%.3f" % elapsed)
PY
  )" || fail "ci-query exceeded its positive wait budget after a pending response"
  [ "$(cat "$counter")" = 2 ] || fail "ci-query did not exercise the slow second request"
  python3 - "$elapsed" <<'PY' || fail "ci-query positive-budget wait exceeded its margin"
import sys
assert float(sys.argv[1]) <= 11.8
PY
  rm -f "$counter"
  result="$(OMS_GH_BIN="$gh" OMS_TEST_CI_COUNTER="$counter" OMS_TEST_CI_DELAY=1.2 \
    python3 "$ROOT/scripts/lib/ci-query.py" \
    https://github.com/example/repo.git "$sha" 0 "" test.yml)" ||
    fail "ci-query timed out a successful response with a zero poll budget"
  [ "$result" = '42 success' ] || fail "zero-budget ci-query missed successful evidence: $result"
  [ "$(cat "$counter")" = 1 ] || fail "zero-budget ci-query made more than one request"
}

test_schema2_auto_update_apply_skips_dirty_and_diverged() {
  local repo="$TMP/schema2-skip-auto"
  local home="$TMP/schema2-skip-home"
  local receipt="$home/.config/oh-my-setting/install.json"
  local state="$home/auto-update.status"
  local marker="$home/apply-called"
  local base target current holder i long_green long_tip step

  mkdir -p "$repo/scripts/lib" "$home/.config/oh-my-setting"
  cp "$ROOT/scripts/auto-update.sh" "$repo/scripts/auto-update.sh"
  cat > "$repo/scripts/lib/ci-query.py" <<'PY'
import os, sys
sha = sys.argv[2]
result = os.environ.get("OMS_TEST_CI_RESULT", "success")
if sha == os.environ.get("OMS_TEST_CI_TIP"):
    result = "cancelled"
elif sha == os.environ.get("OMS_TEST_CI_GREEN"):
    result = "success"
print("1 " + result)
PY
  cp "$ROOT/scripts/lib/file-lock.sh" "$repo/scripts/lib/file-lock.sh"
  cp "$ROOT/scripts/lib/poll.sh" "$repo/scripts/lib/poll.sh"
  cat > "$repo/scripts/update.sh" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = --check ]; then
  current="$(git rev-parse HEAD)"
  printf 'current: %s\n' "${current:0:7}"
  printf 'update-check: available %s -> %s\n' "$current" "$OMS_TEST_UPDATE_TARGET"
  exit 0
fi
if [ -n "${OMS_TEST_UPDATE_APPLY_TARGET:-}" ] &&
   [ "${OH_MY_SETTING_UPDATE_EXPECTED_TARGET:-}" != "$OMS_TEST_UPDATE_APPLY_TARGET" ]; then
  echo "error: update target changed after preflight: expected ${OH_MY_SETTING_UPDATE_EXPECTED_TARGET:-none}, actual $OMS_TEST_UPDATE_APPLY_TARGET" >&2
  exit 75
fi
if [ -n "${OH_MY_SETTING_UPDATE_TARGET_COMMIT:-}" ]; then
  printf 'apply-target=%s\n' "$OH_MY_SETTING_UPDATE_TARGET_COMMIT" >> "$OMS_TEST_UPDATE_MARKER"
else
  printf 'apply\n' >> "$OMS_TEST_UPDATE_MARKER"
fi
EOF
  chmod +x "$repo/scripts/auto-update.sh" "$repo/scripts/update.sh"
  git -C "$repo" init -q
  git -C "$repo" checkout -qb main
  git -C "$repo" config user.name test
  git -C "$repo" config user.email test@example.com
  printf 'base\n' > "$repo/value"
  git -C "$repo" add .
  git -C "$repo" commit -qm base
  base="$(git -C "$repo" rev-parse HEAD)"

  git -C "$repo" checkout -qb upstream "$base"
  printf 'upstream\n' > "$repo/value"
  git -C "$repo" commit -qam upstream
  target="$(git -C "$repo" rev-parse HEAD)"
  git -C "$repo" checkout -q main
  printf 'local\n' > "$repo/value"
  git -C "$repo" commit -qam local
  current="$(git -C "$repo" rev-parse HEAD)"

  python3 - "$receipt" "$repo" "$current" <<'PY'
import json, sys
json.dump({"schema": 2, "source_root": sys.argv[2], "commit": sys.argv[3],
           "channel": "main", "profile": "custom", "ref": "main",
           "components": {}, "managed_targets": [], "plugin": {}},
          open(sys.argv[1], "w"))
PY

  printf 'dirty\n' > "$repo/uncommitted"
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_AUTO_UPDATE_STATE="$state" OH_MY_SETTING_AUTO_UPDATE_LOG="$home/auto.log" \
    OMS_TEST_UPDATE_TARGET="$target" OMS_TEST_UPDATE_MARKER="$marker" \
    "$repo/scripts/auto-update.sh" apply > "$home/dirty.out"
  grep -Fq 'auto-update: skipped' "$home/dirty.out" ||
    fail "schema-2 auto-update did not skip a dirty checkout"
  grep -Fq 'status=skipped' "$state" || fail "dirty schema-2 skip was not recorded"
  [ ! -e "$marker" ] || fail "dirty schema-2 auto-update invoked the mutating updater"

  rm -f "$repo/uncommitted"
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_AUTO_UPDATE_STATE="$state" OH_MY_SETTING_AUTO_UPDATE_LOG="$home/auto.log" \
    OMS_TEST_UPDATE_TARGET="$target" OMS_TEST_UPDATE_MARKER="$marker" \
    "$repo/scripts/auto-update.sh" apply > "$home/diverged.out"
  grep -Fq 'auto-update: skipped' "$home/diverged.out" ||
    fail "schema-2 auto-update did not skip a diverged checkout"
  grep -Fq 'diverged' "$state" || fail "diverged schema-2 skip reason was not recorded"
  [ ! -e "$marker" ] || fail "diverged schema-2 auto-update invoked the mutating updater"

  # A schema-2 receipt used to return before the legacy lock path, so two
  # timers could overlap the checkout/receipt transaction. Hold the shared
  # apply lock and prove this path observes the same non-blocking boundary.
  git -C "$repo" reset -q --hard "$base"
  rm -f "$marker" "$home/lock-ready"
  HOME="$home" OMS_LOCK_DIR="$home/locks" OMS_LOCK_FORCE_MKDIR=1 \
    bash -c '. "$1"; oms_hold_file_lock "$2" 7 || exit; : > "$3"; sleep 10' \
      _ "$repo/scripts/lib/file-lock.sh" "$repo/local/auto-update.apply" \
      "$home/lock-ready" &
  holder=$!
  i=0
  while [ ! -f "$home/lock-ready" ] && [ "$i" -lt 100 ]; do
    sleep 0.05
    i=$((i + 1))
  done
  [ -f "$home/lock-ready" ] || { kill "$holder" 2>/dev/null || true; fail "lock holder did not start"; }
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_AUTO_UPDATE_STATE="$state" OH_MY_SETTING_AUTO_UPDATE_LOG="$home/auto.log" \
    OMS_TEST_UPDATE_TARGET="$target" OMS_TEST_UPDATE_MARKER="$marker" \
    OMS_LOCK_DIR="$home/locks" OMS_LOCK_FORCE_MKDIR=1 \
    "$repo/scripts/auto-update.sh" apply > "$home/locked.out"
  kill "$holder" 2>/dev/null || true
  wait "$holder" 2>/dev/null || true
  grep -Fq 'auto-update: skipped (another run in progress)' "$home/locked.out" ||
    fail "schema-2 auto-update ignored the shared apply lock"
  [ ! -e "$marker" ] || fail "locked schema-2 auto-update invoked the mutating updater"

  # The ref is resolved twice (preflight and transactional update). If it is
  # force-moved between them, the second target must not replace the commit
  # whose ancestry was actually checked.
  rm -f "$marker"
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_AUTO_UPDATE_STATE="$state" OH_MY_SETTING_AUTO_UPDATE_LOG="$home/auto.log" \
    OMS_TEST_UPDATE_TARGET="$target" OMS_TEST_UPDATE_APPLY_TARGET="$current" \
    OMS_TEST_UPDATE_MARKER="$marker" OMS_LOCK_DIR="$home/locks" OMS_LOCK_FORCE_MKDIR=1 \
    "$repo/scripts/auto-update.sh" apply > "$home/retargeted.out"
  grep -Fq 'auto-update: skipped (target changed during preflight)' "$home/retargeted.out" ||
    fail "schema-2 auto-update did not skip a ref changed after ancestry preflight"
  grep -Fq 'status=skipped' "$state" || fail "retargeted schema-2 skip was not recorded"
  [ ! -e "$marker" ] || fail "retargeted schema-2 auto-update applied an unchecked target"

  # The installed HEAD is the merge's second parent. A green first-parent
  # commit is therefore not a fast-forward candidate and must not be selected.
  git -C "$repo" branch installed-head "$current"
  git -C "$repo" checkout -qb merge-target "$target"
  git -C "$repo" merge --no-ff -s ours -m 'fixture: merge installed second parent' installed-head >/dev/null
  local merge_target
  merge_target="$(git -C "$repo" rev-parse HEAD)"
  git -C "$repo" checkout -q installed-head
  rm -f "$marker"
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_AUTO_UPDATE_STATE="$state" OH_MY_SETTING_AUTO_UPDATE_LOG="$home/auto.log" \
    OMS_TEST_UPDATE_TARGET="$merge_target" OMS_TEST_UPDATE_MARKER="$marker" \
    OMS_TEST_CI_TIP="$merge_target" OMS_TEST_CI_GREEN="$target" \
    "$repo/scripts/auto-update.sh" apply > "$home/second-parent.out"
  grep -Fq 'auto-update: skipped' "$home/second-parent.out" ||
    fail "schema-2 auto-update selected a green but diverged first-parent commit"
  grep -Fq 'no green descendant' "$state" ||
    fail "second-parent merge skip did not name the missing fast-forward candidate"
  [ ! -e "$marker" ] ||
    fail "schema-2 auto-update tried to apply a non-descendant green candidate"
  [ "$(git -C "$repo" rev-parse HEAD)" = "$current" ] ||
    fail "second-parent merge skip changed the installed HEAD"

  # Keep a long first-parent history bounded and name that bound in the
  # persisted attention reason when the only green commit is beyond it.
  git -C "$repo" checkout -qb bounded-history "$current"
  for step in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21; do
    printf 'history %s\n' "$step" >> "$repo/value"
    git -C "$repo" add value
    git -C "$repo" commit -qm "fixture: bounded history $step"
    if [ "$step" = 1 ]; then long_green="$(git -C "$repo" rev-parse HEAD)"; fi
  done
  long_tip="$(git -C "$repo" rev-parse HEAD)"
  git -C "$repo" checkout -q installed-head
  rm -f "$marker"
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_AUTO_UPDATE_STATE="$state" OH_MY_SETTING_AUTO_UPDATE_LOG="$home/auto.log" \
    OMS_TEST_UPDATE_TARGET="$long_tip" OMS_TEST_UPDATE_MARKER="$marker" \
    OMS_TEST_CI_RESULT=cancelled OMS_TEST_CI_TIP="$long_tip" OMS_TEST_CI_GREEN="$long_green" \
    "$repo/scripts/auto-update.sh" apply > "$home/bounded-history.out"
  grep -Fq 'auto-update: skipped' "$home/bounded-history.out" ||
    fail "bounded fallback history did not skip when no scanned commit was green"
  grep -Fq 'fallback scan limit reached (20 commits)' "$state" ||
    fail "bounded fallback history did not report its scan limit in the attention state"
  [ ! -e "$marker" ] || fail "bounded fallback history applied an unscanned commit"
  [ "$(git -C "$repo" rev-parse HEAD)" = "$current" ] ||
    fail "bounded fallback history changed the installed HEAD"
}

test_missing_codex_degrades_and_dead_ref_is_fail_closed() {
  local source="$TMP/degrade-source"
  local installed="$TMP/degrade-installed"
  local home="$TMP/degrade-home"
  local receipt="$home/.config/oh-my-setting/install.json"
  local first next before_dead_ref out rc=0

  git clone -q "$ROOT" "$source"
  git -C "$source" checkout -qB main
  cp "$ROOT/scripts/update.sh" "$source/scripts/update.sh"
  cp "$ROOT/scripts/link.sh" "$source/scripts/link.sh"
  cp "$ROOT/scripts/unlink.sh" "$source/scripts/unlink.sh"
  cp "$ROOT/scripts/doctor.sh" "$source/scripts/doctor.sh"
  cp "$ROOT/tools.lock.json" "$source/tools.lock.json"
  cp "$ROOT/scripts/lib/tool-lock.py" "$source/scripts/lib/tool-lock.py"
  cp "$ROOT/scripts/install-claude-hooks.sh" "$source/scripts/install-claude-hooks.sh"
  cp "$ROOT/scripts/lib/claude-hook-surfaces.py" "$source/scripts/lib/claude-hook-surfaces.py"
  cp "$ROOT/scripts/claude-statusline.py" "$source/scripts/claude-statusline.py"
  cp "$ROOT/scripts/claude-subagent-statusline.py" "$source/scripts/claude-subagent-statusline.py"
  cp "$ROOT/scripts/telemetry-hook.sh" "$source/scripts/telemetry-hook.sh"
  cp "$ROOT/scripts/precompact-handoff.sh" "$source/scripts/precompact-handoff.sh"
  cp "$ROOT/scripts/resume-hook.sh" "$source/scripts/resume-hook.sh"
  cp "$ROOT/scripts/install-mcp.sh" "$source/scripts/install-mcp.sh"
  cp "$ROOT/scripts/install-agy-plugin.sh" "$source/scripts/install-agy-plugin.sh"
  cp "$ROOT/scripts/oms-mcp-server.py" "$source/scripts/oms-mcp-server.py"
  cp "$ROOT/scripts/lib/install-contract.sh" "$source/scripts/lib/install-contract.sh"
  cp "$ROOT/scripts/lib/install-lifecycle-lock.sh" "$source/scripts/lib/install-lifecycle-lock.sh"
  cp "$ROOT/scripts/lib/file-lock.sh" "$source/scripts/lib/file-lock.sh"
  cp "$ROOT/scripts/lib/poll.sh" "$source/scripts/lib/poll.sh"
  cp "$ROOT/scripts/lib/platform.sh" "$source/scripts/lib/platform.sh"
  cp "$ROOT/scripts/lib/managed-target.py" "$source/scripts/lib/managed-target.py"
  cp "$ROOT/scripts/lib/agent-install-state.sh" "$source/scripts/lib/agent-install-state.sh"
  git -C "$source" add -A
  git -C "$source" commit -qm "fixture: degrade base" || true
  first="$(git -C "$source" rev-parse HEAD)"

  git clone -q "$source" "$installed"
  mkdir -p "$home"
  # The receipt wants the codex plugin; the restricted PATH below has no
  # codex binary. That exact pair held a real machine at an old commit
  # through daily apply -> fail -> rollback.
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_PROFILE=minimal OH_MY_SETTING_REF=edge \
    OH_MY_SETTING_CLAUDE_HOOKS=0 OH_MY_SETTING_CODEX_PLUGIN=1 \
    OH_MY_SETTING_AUTO_UPDATE=0 "$installed/scripts/link.sh" >/dev/null

  printf 'advance\n' > "$source/advance-marker"
  git -C "$source" add advance-marker
  git -C "$source" commit -qm "fixture: advance"
  next="$(git -C "$source" rev-parse HEAD)"

  # env -u NVM_DIR: the doctor widens PATH via $HOME/.local/bin and NVM_DIR;
  # an inherited real NVM_DIR would resurface the dev machine's npm-installed
  # codex inside the fixture and the guard under test would never fire.
  env -u NVM_DIR HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    PATH="/usr/bin:/bin" "$installed/scripts/update.sh" --no-tools \
    >"$TMP/degrade.out" 2>&1 ||
    { cat "$TMP/degrade.out" >&2; fail "missing codex must not fail the core update"; }
  [ "$(git -C "$installed" rev-parse HEAD)" = "$next" ] ||
    fail "degraded update did not reach the new commit"
  grep -Fq 'codex plugin refresh skipped' "$TMP/degrade.out" ||
    fail "the degradation must be named, not silent"
  [ "$(python3 -c 'import json,sys; print(str(json.load(open(sys.argv[1]))["components"]["codex_plugin"]).lower())' "$receipt")" = true ] ||
    fail "degradation must keep the codex_plugin intent in the receipt"

  # A pin that resolves nowhere is an integrity boundary: an unattended run
  # must not silently switch channels. The operator may choose the origin
  # default branch for one run, but only through an explicit flag.
  printf 'advance again\n' >> "$source/advance-marker"
  git -C "$source" add advance-marker
  git -C "$source" commit -qm "fixture: advance again"
  next="$(git -C "$source" rev-parse HEAD)"
  before_dead_ref="$(git -C "$installed" rev-parse HEAD)"
  rc=0
  out="$(env -u NVM_DIR HOME="$home" XDG_CONFIG_HOME="$home/.config" \
    OMS_INSTALL_RECEIPT="$receipt" PATH="/usr/bin:/bin" \
    "$installed/scripts/update.sh" --ref ghost-branch --no-tools 2>&1)" || rc=$?
  [ "$rc" -ne 0 ] || fail "a dead pinned ref followed the default branch without consent: $out"
  [ "$(git -C "$installed" rev-parse HEAD)" = "$before_dead_ref" ] ||
    fail "a dead pinned ref changed the installed commit"
  printf '%s' "$out" | grep >/dev/null -F 'cannot resolve pinned install ref: ghost-branch' ||
    fail "the dead-ref failure did not name the pin: $out"
  printf '%s' "$out" | grep >/dev/null -F -- '--fallback-to-edge' ||
    fail "the dead-ref failure did not name the explicit recovery: $out"

  env -u NVM_DIR HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    PATH="/usr/bin:/bin" "$installed/scripts/update.sh" --ref ghost-branch \
    --fallback-to-edge --no-tools >"$TMP/deadref.out" 2>&1 ||
    { cat "$TMP/deadref.out" >&2; fail "explicit dead-ref fallback must follow the default branch"; }
  [ "$(git -C "$installed" rev-parse HEAD)" = "$next" ] ||
    fail "explicit dead-ref fallback did not follow the default branch"
  grep -Fq -- '--fallback-to-edge follows default branch' "$TMP/deadref.out" ||
    fail "the explicit dead-ref fallback must be named"
}

test_auto_update_failure_message_names_the_error() {
  local repo="$TMP/errmsg-auto"
  local home="$TMP/errmsg-home"
  local receipt="$home/.config/oh-my-setting/install.json"
  local commit state

  mkdir -p "$repo/scripts/lib" "$repo/local" "$home/.config/oh-my-setting"
  cp "$ROOT/scripts/auto-update.sh" "$repo/scripts/auto-update.sh"
  printf 'import os; print("1 " + os.environ.get("OMS_TEST_CI_RESULT", "success"))\n' > "$repo/scripts/lib/ci-query.py"
  cp "$ROOT/scripts/lib/file-lock.sh" "$repo/scripts/lib/file-lock.sh"
  cp "$ROOT/scripts/lib/poll.sh" "$repo/scripts/lib/poll.sh"
  cp "$ROOT/scripts/lib/install-contract.sh" "$repo/scripts/lib/install-contract.sh"
  cp "$ROOT/scripts/lib/platform.sh" "$repo/scripts/lib/platform.sh"
  cp "$ROOT/scripts/lib/managed-target.py" "$repo/scripts/lib/managed-target.py"
  # An update that fails the way the real one did: a plain error line that
  # the old state message ("receipt ref apply failed") used to bury.
  cat > "$repo/scripts/update.sh" <<'EOF'
#!/usr/bin/env bash
echo "linking things"
echo "error: codex command is required" >&2
exit 1
EOF
  chmod +x "$repo/scripts/auto-update.sh" "$repo/scripts/update.sh"
  git -C "$repo" init -q
  git -C "$repo" checkout -qb main
  git -C "$repo" add .
  git -C "$repo" commit -qm "fixture: errmsg auto update"
  commit="$(git -C "$repo" rev-parse HEAD)"
  python3 - "$receipt" "$repo" "$commit" <<'PY'
import json, sys
json.dump({"schema": 2, "source_root": sys.argv[2], "commit": sys.argv[3],
           "channel": "main", "profile": "custom", "ref": "main",
           "components": {}, "managed_targets": [], "plugin": {}},
          open(sys.argv[1], "w"))
PY
  state="$repo/local/auto-update.status"

  if HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    "$repo/scripts/auto-update.sh" apply >/dev/null 2>&1; then
    fail "failing update must fail the apply run"
  fi
  grep -Fq 'message=apply failed: error: codex command is required' "$state" ||
    fail "apply state message must carry the real error: $(grep '^message=' "$state")"

  if HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    "$repo/scripts/auto-update.sh" check >/dev/null 2>&1; then
    fail "failing update must fail the check run"
  fi
  grep -Fq 'message=check failed: error: codex command is required' "$state" ||
    fail "check state message must carry the real error: $(grep '^message=' "$state")"
  [ "$(grep -c '^message=' "$state")" = 1 ] ||
    fail "a multi-line failure must not corrupt the key=value state file"
}

test_harness_child_cannot_mutate_install
test_schema1_receipt_migrates_to_profiled_schema2
test_update_rolls_back_and_supports_explicit_rollback
test_doctor_failure_restores_previous_plugin_payload
test_schema1_update_preserves_channel_pin_and_cron
test_signal_during_doctor_rolls_back_transaction
test_detached_schema2_auto_update_check
test_schema1_auto_update_reuses_update_transaction
test_ci_query_waits_for_pending_run
test_schema2_auto_update_apply_skips_dirty_and_diverged
test_missing_codex_degrades_and_dead_ref_is_fail_closed
test_auto_update_failure_message_names_the_error
echo "update-v04-smoke: ok"
