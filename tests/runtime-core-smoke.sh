#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export PYTHONDONTWRITEBYTECODE=1
PYTHONPATH="$ROOT/scripts/lib" python3 -m unittest discover -v -s "$ROOT/tests" -p "test_oms_runtime_*.py"

# Exercise the real shell entrypoint and JSON surface, not only imports.
tmp="$(mktemp -d "${TMPDIR:-/tmp}/oms-runtime-cli.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT INT TERM HUP
git -C "$tmp" init -q
git -C "$tmp" config user.email test@example.com
git -C "$tmp" config user.name test
cat > "$tmp/PROJECT.md" <<'EOF'
# Fixture

## Goal

Check the CLI.

## Acceptance Criteria

- [id:cli-json] Envelope is JSON.
EOF
git -C "$tmp" add PROJECT.md
git -C "$tmp" commit -qm fixture
"$ROOT/scripts/runtime.sh" --repo "$tmp" envelope show | python3 -c 'import json,sys; row=json.load(sys.stdin); assert row["schema"]==2'
"$ROOT/scripts/runtime.sh" --repo "$tmp" failure classify 'verification failed' | python3 -c 'import json,sys; assert json.load(sys.stdin)["code"]=="verifier_failed"'
"$ROOT/scripts/runtime.sh" --repo "$tmp" backend run trusted-local --timeout-seconds 10 -- python3 -c 'print("ok")' | python3 -c 'import json,sys; row=json.load(sys.stdin); assert row["exit"]==0'

assert_child_runtime_refused() {
  label="$1"
  shift
  output="$tmp/child-$label.out"
  rc=0
  OMS_HARNESS_CHILD=1 "$@" >"$output" 2>&1 || rc=$?
  [ "$rc" -eq 2 ] || {
    echo "FAIL: $label child runtime mutation returned $rc: $(cat "$output")" >&2
    exit 1
  }
  grep -Fq 'a harness child may only use read-only runtime actions' "$output" || {
    echo "FAIL: $label child refusal was not actionable: $(cat "$output")" >&2
    exit 1
  }
}

# The semantic guard must hold at all three supported entrypoints before a
# backend command or output writer gets a chance to run.
for entrypoint in oms runtime core; do
  sentinel="$tmp/$entrypoint-ran"
  case "$entrypoint" in
    oms)
      assert_child_runtime_refused "$entrypoint" "$ROOT/scripts/oms" runtime \
        --repo "$tmp" backend run trusted-local -- python3 -c \
        'import pathlib,sys; pathlib.Path(sys.argv[1]).touch()' "$sentinel"
      ;;
    runtime)
      assert_child_runtime_refused "$entrypoint" "$ROOT/scripts/runtime.sh" \
        --repo "$tmp" backend run trusted-local -- python3 -c \
        'import pathlib,sys; pathlib.Path(sys.argv[1]).touch()' "$sentinel"
      ;;
    core)
      assert_child_runtime_refused "$entrypoint" python3 \
        "$ROOT/scripts/lib/oms_core.py" --repo "$tmp" \
        backend run trusted-local -- python3 -c \
        'import pathlib,sys; pathlib.Path(sys.argv[1]).touch()' "$sentinel"
      ;;
  esac
  [ ! -e "$sentinel" ] || {
    echo "FAIL: $entrypoint child runtime executed its backend command" >&2
    exit 1
  }
done

blocked_output="$tmp/blocked-envelope.json"
assert_child_runtime_refused output "$ROOT/scripts/runtime.sh" --repo "$tmp" \
  envelope write --output "$blocked_output"
[ ! -e "$blocked_output" ] || {
  echo "FAIL: child runtime wrote an envelope" >&2
  exit 1
}

# Read/probe operations, dry-run installation planning, and parser-owned help
# remain available to a delegated worker.
OMS_HARNESS_CHILD=1 "$ROOT/scripts/runtime.sh" --repo "$tmp" envelope show >/dev/null
OMS_HARNESS_CHILD=1 "$ROOT/scripts/runtime.sh" --repo "$tmp" \
  profile install core --primary-provider codex --dry-run >/dev/null
OMS_HARNESS_CHILD=1 "$ROOT/scripts/runtime.sh" --help >/dev/null
OMS_HARNESS_CHILD=1 "$ROOT/scripts/runtime.sh" --version >/dev/null

assert_child_host_refused() {
  label="$1"
  shift
  output="$tmp/host-$label.out"
  rc=0
  OMS_HARNESS_CHILD=1 "$@" >"$output" 2>&1 || rc=$?
  [ "$rc" -eq 2 ] || {
    echo "FAIL: $label child host mutation returned $rc: $(cat "$output")" >&2
    exit 1
  }
  grep -Fq 'a harness child cannot mutate OMS host lifecycle authority' "$output" || {
    echo "FAIL: $label child host refusal was not actionable: $(cat "$output")" >&2
    exit 1
  }
}

assert_child_global_refused() {
  label="$1"
  shift
  output="$tmp/global-$label.out"
  rc=0
  OMS_HARNESS_CHILD=1 "$@" >"$output" 2>&1 || rc=$?
  [ "$rc" -eq 2 ] || {
    echo "FAIL: $label child global mutation returned $rc: $(cat "$output")" >&2
    exit 1
  }
  grep -Fq 'a harness child cannot mutate parent-owned host or global state' "$output" || {
    echo "FAIL: $label child global refusal was not actionable: $(cat "$output")" >&2
    exit 1
  }
}

# Runtime aliases are not the only public route to host lifecycle state. Use
# isolated/copy fixtures so this regression is safe even against the old,
# unguarded implementation: it can fail, but cannot touch the real install.
host_fixture="$tmp/host-authority"
host_user_root="$host_fixture/user-root"
mkdir -p "$host_fixture/auto/scripts" "$host_fixture/uninstall/scripts" \
  "$host_fixture/nonrepo" "$host_user_root" "$host_fixture/tmp" \
  "$host_fixture/locks" "$host_fixture/worktrees" "$host_fixture/bin"
cp "$ROOT/scripts/auto-update.sh" "$host_fixture/auto/scripts/auto-update.sh"
cp "$ROOT/scripts/uninstall.sh" "$host_fixture/uninstall/scripts/uninstall.sh"
chmod +x "$host_fixture/auto/scripts/auto-update.sh" \
  "$host_fixture/uninstall/scripts/uninstall.sh"
printf '%s\n' '#!/usr/bin/env bash' \
  'case "$*" in *--version*) echo "fixture 1.0" ;; *) echo "fixture help" ;; esac' \
  > "$host_fixture/bin/codex"
cp "$host_fixture/bin/codex" "$host_fixture/bin/claude"
cp "$host_fixture/bin/codex" "$host_fixture/bin/agy"
chmod +x "$host_fixture/bin/codex" "$host_fixture/bin/claude" \
  "$host_fixture/bin/agy"

assert_child_auto_refused() {
  label="$1"
  shift
  assert_child_host_refused "$label" env \
    HOME="$host_user_root" XDG_CONFIG_HOME="$host_user_root/.config" \
    OH_MY_SETTING_AUTO_UPDATE_STATE="$host_fixture/auto-state.json" \
    OH_MY_SETTING_AUTO_UPDATE_LOG="$host_fixture/auto-update.log" \
    OMS_INSTALL_RECEIPT="$host_fixture/install-receipt.json" \
    OMS_LOCK_DIR="$host_fixture/locks" \
    "$host_fixture/auto/scripts/auto-update.sh" "$@"
}

assert_child_auto_refused auto-default
assert_child_auto_refused auto-check check
assert_child_auto_refused auto-apply apply
assert_child_auto_refused auto-install install
assert_child_auto_refused auto-remove remove
assert_child_host_refused doctor-repair env \
  HOME="$host_user_root" XDG_CONFIG_HOME="$host_user_root/.config" \
  CODEX_HOME="$host_user_root/.codex" \
  OMS_CODEX_CONFIG="$host_user_root/.codex/config.toml" \
  OMS_CLAUDE_SETTINGS="$host_user_root/.claude/settings.json" \
  OMS_LOCK_DIR="$host_fixture/locks" \
  OMS_INSTALL_RECEIPT="$host_fixture/doctor-receipt.json" \
  OH_MY_SETTING_CLAUDE_HOOKS=0 OH_MY_SETTING_CODEX_PLUGIN=0 \
  OH_MY_SETTING_AUTO_UPDATE=0 OH_MY_SETTING_GENERATE_MACHINE=0 \
  OH_MY_SETTING_GENERATE_SLURM=0 \
  "$ROOT/scripts/doctor.sh" --repair --no-model-doctor
assert_child_host_refused uninstall env \
  HOME="$host_user_root" XDG_CONFIG_HOME="$host_user_root/.config" \
  CODEX_HOME="$host_user_root/.codex" \
  OMS_INSTALL_RECEIPT="$host_fixture/install-receipt.json" \
  OMS_LOCK_DIR="$host_fixture/locks" \
  "$host_fixture/uninstall/scripts/uninstall.sh" --yes
assert_child_host_refused permissions-apply env \
  HOME="$host_user_root" XDG_CONFIG_HOME="$host_user_root/.config" \
  OMS_LOCK_DIR="$host_fixture/locks" \
  "$ROOT/scripts/provider-permissions.sh" --apply --profile consult \
  --settings "$host_fixture/permissions.json" \
  --worktree-parent "$host_fixture/worktrees"
assert_child_host_refused permissions-remove env \
  HOME="$host_user_root" XDG_CONFIG_HOME="$host_user_root/.config" \
  OMS_LOCK_DIR="$host_fixture/locks" \
  "$ROOT/scripts/provider-permissions.sh" --remove --profile consult \
  --settings "$host_fixture/permissions.json" \
  --worktree-parent "$host_fixture/worktrees"
assert_child_host_refused cleanup-apply env \
  HOME="$host_user_root" XDG_CACHE_HOME="$host_user_root/.cache" \
  TMPDIR="$host_fixture/tmp" OMS_LOCK_DIR="$host_fixture/locks" \
  OMS_DELEGATE_WORKTREE_ROOT="$host_fixture/worktrees" bash -c \
  'cd "$1" && exec "$2" --apply' child-cleanup \
  "$host_fixture/nonrepo" "$ROOT/scripts/cleanup.sh"
assert_child_global_refused doctor-model-refresh env \
  HOME="$host_user_root" XDG_CACHE_HOME="$host_user_root/.cache" \
  XDG_CONFIG_HOME="$host_user_root/.config" \
  OH_MY_SETTING_MODEL_DOCTOR=auto \
  OMS_CAPABILITY_DIR="$host_fixture/capabilities" \
  PATH="$host_fixture/bin:/usr/bin:/bin" \
  "$ROOT/scripts/doctor.sh"
assert_child_global_refused model-doctor env \
  HOME="$host_user_root" XDG_CACHE_HOME="$host_user_root/.cache" \
  OMS_CAPABILITY_DIR="$host_fixture/capabilities" \
  PATH="$host_fixture/bin:/usr/bin:/bin" \
  "$ROOT/scripts/model-doctor.sh"
assert_child_global_refused models-refresh env \
  HOME="$host_user_root" XDG_CACHE_HOME="$host_user_root/.cache" \
  OMS_CAPABILITY_DIR="$host_fixture/capabilities" \
  PATH="$host_fixture/bin:/usr/bin:/bin" \
  "$ROOT/scripts/models.sh" --refresh
[ ! -e "$host_fixture/capabilities" ] || {
  echo "FAIL: refused model capability refresh wrote routing state" >&2
  exit 1
}

# Observation and final dry-run selections remain usable by child workers.
for action in status attention --help; do
  OMS_HARNESS_CHILD=1 HOME="$host_user_root" \
    XDG_CONFIG_HOME="$host_user_root/.config" \
    OH_MY_SETTING_AUTO_UPDATE_STATE="$host_fixture/auto-state.json" \
    OH_MY_SETTING_AUTO_UPDATE_LOG="$host_fixture/auto-update.log" \
    OMS_INSTALL_RECEIPT="$host_fixture/install-receipt.json" \
    OMS_LOCK_DIR="$host_fixture/locks" \
    "$host_fixture/auto/scripts/auto-update.sh" "$action" >/dev/null
done
OMS_HARNESS_CHILD=1 HOME="$host_user_root" \
  XDG_CONFIG_HOME="$host_user_root/.config" \
  CODEX_HOME="$host_user_root/.codex" \
  OMS_CODEX_CONFIG="$host_user_root/.codex/config.toml" \
  OMS_LOCK_DIR="$host_fixture/locks" \
  "$ROOT/scripts/doctor.sh" --tool-lock >/dev/null
OMS_HARNESS_CHILD=1 HOME="$host_user_root" \
  XDG_CONFIG_HOME="$host_user_root/.config" \
  CODEX_HOME="$host_user_root/.codex" \
  OMS_INSTALL_RECEIPT="$host_fixture/install-receipt.json" \
  OMS_LOCK_DIR="$host_fixture/locks" \
  "$ROOT/scripts/uninstall.sh" --dry-run --yes >/dev/null
OMS_HARNESS_CHILD=1 HOME="$host_user_root" \
  XDG_CONFIG_HOME="$host_user_root/.config" \
  OMS_LOCK_DIR="$host_fixture/locks" \
  "$ROOT/scripts/provider-permissions.sh" --print --profile consult \
  --settings "$host_fixture/permissions.json" \
  --worktree-parent "$host_fixture/worktrees" >/dev/null
OMS_HARNESS_CHILD=1 HOME="$host_user_root" \
  XDG_CACHE_HOME="$host_user_root/.cache" \
  OMS_CAPABILITY_DIR="$host_fixture/capabilities" \
  PATH="$host_fixture/bin:/usr/bin:/bin" \
  "$ROOT/scripts/models.sh" --json >/dev/null
OMS_HARNESS_CHILD=1 "$ROOT/scripts/model-doctor.sh" --help >/dev/null
(
  cd "$host_fixture/nonrepo"
  OMS_HARNESS_CHILD=1 HOME="$host_user_root" TMPDIR="$host_fixture" \
    "$ROOT/scripts/cleanup.sh" --apply --dry-run >/dev/null
)

echo "runtime-core-smoke: ok"
