#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-v04-update.XXXXXX")"
trap '[ "${KEEP_TMP:-0}" = 1 ] || rm -rf "$TMP"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }

export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=oms-test GIT_AUTHOR_EMAIL=test@example.com
export GIT_COMMITTER_NAME=oms-test GIT_COMMITTER_EMAIL=test@example.com

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

  git clone -q "$ROOT" "$source"
  # During development the root may be dirty; copy the transaction files so
  # this fixture always exercises the working implementation.
  cp "$ROOT/scripts/update.sh" "$source/scripts/update.sh"
  cp "$ROOT/scripts/link.sh" "$source/scripts/link.sh"
  cp "$ROOT/scripts/unlink.sh" "$source/scripts/unlink.sh"
  cp "$ROOT/scripts/doctor.sh" "$source/scripts/doctor.sh"
  cp "$ROOT/scripts/lib/install-contract.sh" "$source/scripts/lib/install-contract.sh"
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
    OH_MY_SETTING_AUTO_UPDATE=0 "$installed/scripts/link.sh" >/dev/null
  cp "$receipt" "$before_receipt"
  rm -f "$home/.codex/AGENTS.md"
  printf 'user rules\n' > "$home/.codex/AGENTS.md"
  printf 'older backup\n' > "$home/.codex/AGENTS.md.backup.20260701000000"

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
  grep -Fq "rollback restored $first" "$TMP/fail.out" || fail "rollback was not reported"
  [ ! -L "$home/.codex/AGENTS.md" ] && grep -Fxq 'user rules' "$home/.codex/AGENTS.md" ||
    fail "failed update did not restore the user-owned rules file"
  [ "$(find "$home/.codex" -maxdepth 1 -name 'AGENTS.md.backup.*' | wc -l | tr -d ' ')" = 1 ] ||
    fail "failed update changed the pre-existing backup set"
  grep -Fxq 'older backup' "$home/.codex/AGENTS.md.backup.20260701000000" ||
    fail "failed update changed the pre-existing backup"
  [ "$failed" != "$first" ] || fail "fixture did not create a failing target"

  rm -f "$home/.codex/AGENTS.md" "$home/.codex/AGENTS.md.backup.20260701000000"
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_PROFILE=minimal OH_MY_SETTING_REF=edge \
    OH_MY_SETTING_CLAUDE_HOOKS=0 OH_MY_SETTING_CODEX_PLUGIN=0 \
    OH_MY_SETTING_AUTO_UPDATE=0 "$installed/scripts/link.sh" >/dev/null

  cp "$installed/scripts/doctor.sh" "$source/scripts/doctor.sh"
  git -C "$source" add scripts/doctor.sh
  git -C "$source" commit -qm "fixture: healthy doctor"
  good="$(git -C "$source" rev-parse HEAD)"
  HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    PATH="/usr/bin:/bin" "$installed/scripts/update.sh" --no-tools >/dev/null
  [ "$(git -C "$installed" rev-parse HEAD)" = "$good" ] || fail "successful update missed target"
  [ "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["previous_commit"])' "$receipt")" = "$first" ] ||
    fail "successful update did not record previous_commit"
  ! grep -Fq 'skill-router.sh' "$home/.claude/settings.json" 2>/dev/null ||
    fail "plain update re-enabled a disabled Claude hook"
  [ ! -e "$home/.config/systemd/user/oh-my-setting-autoupdate.timer" ] ||
    fail "plain update re-enabled a disabled update timer"

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
  local first

  mkdir -p "$source/scripts/lib" "$bin" "$(dirname "$receipt")"
  cp "$ROOT/scripts/update.sh" "$source/scripts/update.sh"
  cp "$ROOT/scripts/lib/install-contract.sh" "$source/scripts/lib/install-contract.sh"
  for script in link install-claude-hooks install-autoupdate uninstall-autoupdate install-tools; do
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

  printf 'new\n' > "$source/plugin-payload"
  printf '#!/usr/bin/env bash\nexit 41\n' > "$source/scripts/doctor.sh"
  git -C "$source" add plugin-payload scripts/doctor.sh
  git -C "$source" commit -qm "fixture: new plugin with failing doctor"

  if HOME="$home" XDG_CONFIG_HOME="$home/.config" OMS_INSTALL_RECEIPT="$receipt" \
    OMS_PLUGIN_STATE="$state" PATH="$bin:/usr/bin:/bin" \
    "$installed/scripts/update.sh" --no-tools >/dev/null 2>&1; then
    fail "plugin fixture update should fail doctor"
  fi
  grep -Fxq old "$state" || fail "rollback left the target plugin payload installed"
  [ "$(git -C "$installed" rev-parse HEAD)" = "$first" ] ||
    fail "plugin rollback did not restore source HEAD"
}

test_schema1_update_preserves_channel_pin_and_cron() {
  local source="$TMP/schema1-source"
  local installed="$TMP/schema1-installed"
  local detached="$TMP/schema1-detached"
  local home="$TMP/schema1-update-home"
  local detached_home="$TMP/schema1-detached-home"
  local receipt="$home/.config/oh-my-setting/install.json"
  local detached_receipt="$detached_home/.config/oh-my-setting/install.json"
  local cron_file="$home/cron.txt"
  local base
  local release_head

  git clone -q "$ROOT" "$source"
  for file in scripts/update.sh scripts/link.sh scripts/doctor.sh \
    scripts/lib/install-contract.sh scripts/lib/agent-install-state.sh; do
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
  cat > "$source/scripts/link.sh" <<'EOF'
#!/usr/bin/env bash
mkdir -p "$HOME/.codex"
rm -f "$HOME/.codex/AGENTS.md"
ln -s /target/rules "$HOME/.codex/AGENTS.md"
EOF
  for script in install-claude-hooks install-codex-plugin install-autoupdate uninstall-autoupdate install-tools; do
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
  local repo="$TMP/detached-auto"
  local home="$TMP/detached-auto-home"
  local receipt="$home/.config/oh-my-setting/install.json"
  local commit

  mkdir -p "$repo/scripts/lib" "$repo/local" "$home/.config/oh-my-setting"
  cp "$ROOT/scripts/auto-update.sh" "$repo/scripts/auto-update.sh"
  cp "$ROOT/scripts/lib/install-contract.sh" "$repo/scripts/lib/install-contract.sh"
  cat > "$repo/scripts/update.sh" <<'EOF'
#!/usr/bin/env bash
commit="$(git rev-parse HEAD)"
echo "current: ${commit:0:7}"
echo "update-check: up_to_date $commit"
EOF
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
  if grep -Fq 'detached HEAD; auto-update skipped' "$repo/local/auto-update.status"; then
    fail "detached schema-2 check was skipped"
  fi
}

test_schema1_receipt_migrates_to_profiled_schema2
test_update_rolls_back_and_supports_explicit_rollback
test_doctor_failure_restores_previous_plugin_payload
test_schema1_update_preserves_channel_pin_and_cron
test_signal_during_doctor_rolls_back_transaction
test_detached_schema2_auto_update_check
echo "update-v04-smoke: ok"
