#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-lifecycle-hardening.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

assert_contains() {
  grep -Fq -- "$2" "$1" || fail "$1 does not contain: $2"
}

test_receipt_preserves_snapshot_modes() {
  local repo="$TMP/receipt-repo"
  local receipt="$TMP/config/install.json"
  mkdir -p "$repo/plugins/oh-my-setting/.codex-plugin" "$(dirname "$receipt")"
  printf '0.4.0\n' > "$repo/VERSION"
  printf '{"version":"0.4.0"}\n' > "$repo/plugins/oh-my-setting/.codex-plugin/plugin.json"
  git -C "$repo" init -q
  git -C "$repo" config user.email test@example.com
  git -C "$repo" config user.name Test
  git -C "$repo" add .
  git -C "$repo" commit -qm fixture

  HOME="$TMP/home" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_GENERATE_MACHINE=auto OH_MY_SETTING_GENERATE_SLURM=auto \
    bash -c '. "$1"; oms_install_write_receipt "$2" "$3"' _ \
      "$ROOT/scripts/lib/install-contract.sh" "$repo" "$receipt"
  python3 - "$receipt" <<'PY' || fail "receipt lost snapshot mode"
import json, sys
d = json.load(open(sys.argv[1], encoding="utf-8"))
assert d["component_modes"]["machine_snapshot"] == "auto"
assert d["component_modes"]["slurm_snapshot"] == "auto"
assert d["components"]["machine_snapshot"] is True
assert d["components"]["slurm_snapshot"] is True
PY
}

test_machine_snapshot_cli_and_permissions() {
  local out="$TMP/machine.md"
  "$ROOT/scripts/write-machine-snapshot.sh" --help >/dev/null
  OH_MY_SETTING_MACHINE_SNAPSHOT="$out" "$ROOT/scripts/write-machine-snapshot.sh" --dry-run > "$TMP/machine-dry"
  [ ! -e "$out" ] || fail "machine --dry-run wrote output"
  assert_contains "$TMP/machine-dry" "Schema: 1"
  OH_MY_SETTING_MACHINE_SNAPSHOT="$out" "$ROOT/scripts/write-machine-snapshot.sh" >/dev/null
  [ "$(stat -c '%a' "$out" 2>/dev/null || stat -f '%Lp' "$out")" = 600 ] ||
    fail "machine snapshot is not private"
  OH_MY_SETTING_MACHINE_SNAPSHOT="$out" "$ROOT/scripts/write-machine-snapshot.sh" --check >/dev/null
  printf 'broken\n' > "$out"
  if OH_MY_SETTING_MACHINE_SNAPSHOT="$out" "$ROOT/scripts/write-machine-snapshot.sh" --check >/dev/null 2>&1; then
    fail "machine snapshot check accepted corrupt content"
  fi
}

test_slurm_snapshot_cli_and_permissions() {
  local bin="$TMP/slurm-bin"
  local out="$TMP/cluster.md"
  mkdir -p "$bin"
  cat > "$bin/sinfo" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  *%P*%a*) printf 'gpu*|up|1-00:00:00|1|gpu:a100:1|64000|8|node1\n' ;;
  *%P*%l*) printf 'gpu*|1-00:00:00|1|gpu:a100:1|8|node1\n' ;;
  *%P*) printf 'gpu*\n' ;;
esac
EOF
  chmod +x "$bin/sinfo"
  "$ROOT/scripts/generate-slurm-skill.sh" --help >/dev/null
  PATH="$bin:/usr/bin:/bin" OH_MY_SETTING_SLURM_REF="$out" \
    "$ROOT/scripts/generate-slurm-skill.sh" --dry-run > "$TMP/slurm-dry"
  [ ! -e "$out" ] || fail "slurm --dry-run wrote output"
  assert_contains "$TMP/slurm-dry" "Schema: 1"
  PATH="$bin:/usr/bin:/bin" OH_MY_SETTING_SLURM_REF="$out" \
    "$ROOT/scripts/generate-slurm-skill.sh" >/dev/null
  [ "$(stat -c '%a' "$out" 2>/dev/null || stat -f '%Lp' "$out")" = 600 ] ||
    fail "Slurm snapshot is not private"
  OH_MY_SETTING_SLURM_REF="$out" "$ROOT/scripts/generate-slurm-skill.sh" --check >/dev/null
}

test_project_doctor_strict_slurm_contract() {
  local project="$TMP/slurm-project"
  mkdir -p "$project"
  "$ROOT/scripts/apply-project-template.sh" slurm "$project" >/dev/null
  sed -i \
    -e 's/^- State: draft/- State: active/' \
    -e 's/^- Test:$/- Test: bash scripts\/check.sh/' \
    -e 's/^- Success criteria:$/- Success criteria: checks pass/' \
    "$project/PROJECT.md"
  "$ROOT/scripts/project-doctor.sh" "$project" > "$TMP/project-doctor"
  assert_contains "$TMP/project-doctor" "Slurm execution contract fields are empty"
  if "$ROOT/scripts/project-doctor.sh" --strict "$project" >/dev/null 2>&1; then
    fail "strict project doctor accepted empty Slurm contract"
  fi
}

test_tsp_fallback_requires_opt_in() {
  local project="$TMP/tsp-project"
  mkdir -p "$project"
  if (cd "$project" && OMS_TSP_FORCE_FALLBACK=1 PATH="/usr/bin:/bin" \
      "$ROOT/scripts/tsp-queue.sh" enqueue -- bash -c true >/dev/null 2>"$TMP/tsp-denied"); then
    fail "missing tsp started a job without opt-in"
  fi
  assert_contains "$TMP/tsp-denied" "--allow-noqueue"
  (cd "$project" && OMS_TSP_FORCE_FALLBACK=1 OMS_TSP_FALLBACK_DIR="$TMP/tsp-fallback" \
    PATH="/usr/bin:/bin" "$ROOT/scripts/tsp-queue.sh" enqueue --allow-noqueue -- bash -c true >/dev/null)
}

test_tool_upgrade_refreshes_existing_clis() {
  local bin="$TMP/tool-bin"
  local home="$TMP/tool-home"
  mkdir -p "$bin" "$home/.local/bin"
  for cmd in node claude codex agy uv; do
    cat > "$bin/$cmd" <<EOF
#!/usr/bin/env bash
if [ "$cmd" = node ] && [ "\${1:-}" = -p ]; then echo 22; else echo '$cmd 1.0'; fi
EOF
    chmod +x "$bin/$cmd"
  done
  cat > "$bin/npm" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$OMS_TEST_NPM_LOG"
[ "$1 $2 $3" = "config get prefix" ] && printf '%s\n' "$OMS_TEST_NPM_PREFIX"
exit 0
EOF
  chmod +x "$bin/npm"
  OMS_TEST_NPM_LOG="$TMP/npm.log" OMS_TEST_NPM_PREFIX="$home" HOME="$home" \
    NVM_DIR="$home/.nvm" OH_MY_SETTING_UPGRADE_ANTIGRAVITY=0 \
    PATH="$bin:/usr/bin:/bin" "$ROOT/scripts/install-tools.sh" --upgrade >/dev/null
  assert_contains "$TMP/npm.log" "install -g @anthropic-ai/claude-code"
  assert_contains "$TMP/npm.log" "install -g @openai/codex"
}

test_branch_receipt_auto_update_uses_transaction() {
  local repo="$TMP/auto-repo"
  local home="$TMP/auto-home"
  local receipt="$home/.config/oh-my-setting/install.json"
  mkdir -p "$repo/scripts" "$repo/local" "$(dirname "$receipt")"
  cp "$ROOT/scripts/auto-update.sh" "$repo/scripts/auto-update.sh"
  cat > "$repo/scripts/update.sh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" > "$OMS_TEST_UPDATE_MARKER"
commit="$(git rev-parse HEAD)"
echo "update-check: up_to_date $commit"
EOF
  chmod +x "$repo/scripts/auto-update.sh" "$repo/scripts/update.sh"
  git -C "$repo" init -q
  git -C "$repo" checkout -qb main
  git -C "$repo" config user.email test@example.com
  git -C "$repo" config user.name Test
  git -C "$repo" add .
  git -C "$repo" commit -qm fixture
  python3 - "$receipt" "$repo" <<'PY'
import json, sys
json.dump({"schema": 2, "source_root": sys.argv[2], "ref": "edge"}, open(sys.argv[1], "w"))
PY
  HOME="$home" OMS_INSTALL_RECEIPT="$receipt" OMS_TEST_UPDATE_MARKER="$TMP/update.marker" \
    "$repo/scripts/auto-update.sh" check >/dev/null
  assert_contains "$TMP/update.marker" "--check"
}

test_update_refreshes_snapshot_policy() {
  local source="$TMP/update-source"
  local remote="$TMP/update-remote.git"
  local installed="$TMP/update-installed"
  local home="$TMP/update-home"
  local receipt="$home/.config/oh-my-setting/install.json"
  local first

  mkdir -p "$source/scripts/lib" "$(dirname "$receipt")"
  cp "$ROOT/scripts/update.sh" "$source/scripts/update.sh"
  cp "$ROOT/scripts/lib/install-contract.sh" "$source/scripts/lib/install-contract.sh"
  for script in link install-claude-hooks install-codex-plugin install-autoupdate uninstall-autoupdate doctor; do
    printf '#!/usr/bin/env bash\nexit 0\n' > "$source/scripts/$script.sh"
    chmod +x "$source/scripts/$script.sh"
  done
  cat > "$source/scripts/write-machine-snapshot.sh" <<'EOF'
#!/usr/bin/env bash
printf 'machine\n' >> "$OMS_TEST_SNAPSHOT_LOG"
EOF
  cat > "$source/scripts/generate-slurm-skill.sh" <<'EOF'
#!/usr/bin/env bash
printf 'slurm\n' >> "$OMS_TEST_SNAPSHOT_LOG"
EOF
  chmod +x "$source/scripts/write-machine-snapshot.sh" "$source/scripts/generate-slurm-skill.sh"
  printf 'base\n' > "$source/README.md"
  git -C "$source" init -q
  git -C "$source" checkout -qb main
  git -C "$source" config user.email test@example.com
  git -C "$source" config user.name Test
  git -C "$source" add .
  git -C "$source" commit -qm base
  first="$(git -C "$source" rev-parse HEAD)"
  git clone -q --bare "$source" "$remote"
  git -C "$source" remote add origin "$remote"
  git clone -q "$remote" "$installed"
  printf 'next\n' >> "$source/README.md"
  git -C "$source" add README.md
  git -C "$source" commit -qm next
  git -C "$source" push -q origin main
  python3 - "$receipt" "$installed" "$first" <<'PY'
import json, sys
json.dump({
  "schema": 2, "source_root": sys.argv[2], "commit": sys.argv[3],
  "channel": "main", "dirty": False, "version": "0.4.0", "profile": "full",
  "ref": "edge", "previous_commit": "", "installed_at": "2026-07-15T00:00:00Z",
  "components": {"tools": False, "claude_hooks": False, "codex_plugin": False,
    "auto_update": False, "machine_snapshot": True, "slurm_snapshot": True},
  "component_modes": {"machine_snapshot": "auto", "slurm_snapshot": "auto"},
  "managed_targets": [],
  "plugin": {"name": "oh-my-setting", "version": "0.4.0", "sha256": "x" * 64}
}, open(sys.argv[1], "w", encoding="utf-8"))
PY
  HOME="$home" OMS_INSTALL_RECEIPT="$receipt" OMS_TEST_SNAPSHOT_LOG="$TMP/snapshot.log" \
    PATH="/usr/bin:/bin" "$installed/scripts/update.sh" --no-tools --no-doctor >/dev/null
  assert_contains "$TMP/snapshot.log" "machine"
  if grep -Fq slurm "$TMP/snapshot.log"; then
    fail "Slurm auto snapshot ran without sinfo"
  fi
  : > "$TMP/snapshot.log"
  HOME="$home" OMS_INSTALL_RECEIPT="$receipt" OMS_TEST_SNAPSHOT_LOG="$TMP/snapshot.log" \
    OH_MY_SETTING_GENERATE_MACHINE=0 OH_MY_SETTING_GENERATE_SLURM=1 \
    PATH="/usr/bin:/bin" "$installed/scripts/update.sh" --no-tools --no-doctor >/dev/null
  assert_contains "$TMP/snapshot.log" "slurm"
  if grep -Fq machine "$TMP/snapshot.log"; then
    fail "explicit machine snapshot disable did not override receipt auto mode"
  fi
}

test_receipt_preserves_snapshot_modes
test_machine_snapshot_cli_and_permissions
test_slurm_snapshot_cli_and_permissions
test_project_doctor_strict_slurm_contract
test_tsp_fallback_requires_opt_in
test_tool_upgrade_refreshes_existing_clis
test_branch_receipt_auto_update_uses_transaction
test_update_refreshes_snapshot_policy
echo "lifecycle-hardening-smoke: ok"
