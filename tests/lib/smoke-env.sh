# shellcheck shell=bash
# Developer UPG shells (umask 0002) would create 0775 lock/state dirs rejected
# by oms-lock's mode & 0o022 == 0 check; match standard restrictive smoke umask.
umask 022
# Fixtures establish their own operator, panel and room identities, even when
# invoked from a managed terminal or a delegated worker.
unset TMUX TMUX_PANE OMS_STATE_REPO OMS_PLAN_LEASE_ID OMS_TASK_ID OMS_ATTEMPT_ID
unset CODEX_THREAD_ID CLAUDE_CODE_SESSION_ID
# An admission or gate verify budget set by the caller is not the fixtures' own.
unset OMS_PEER_VERIFY_TIMEOUT
unset OMS_UPDATE_CALL_REPOS
# Runtime seat-wall and worker-guard knobs of an accepting caller are not the fixtures' own.
unset OMS_PEER_TIMEOUT OMS_PEER_PRINT_TIMEOUT OMS_WORKER_GUARD_STRICT OMS_WORKER_GUARD_OFF
unset NVM_BIN NVM_DIR
# Match CI's hermetic Git configuration and pin a fixture identity so commits
# never depend on ambient developer configuration.
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=oms-test GIT_AUTHOR_EMAIL=test@example.com
export GIT_COMMITTER_NAME=oms-test GIT_COMMITTER_EMAIL=test@example.com
# Config resolution is hermetic too: suites run directly (worker verify, floors)
# must not read or write the invoking user's provider account registry. Keep
# check.sh's runtime config, or the private dir an enclosing suite already made.
case "${XDG_CONFIG_HOME:-}" in
  */oms-check-runtime.*/config) ;;
  *)
    if [ -z "${OMS_SMOKE_CONFIG_HOME:-}" ] || [ "${XDG_CONFIG_HOME:-}" != "$OMS_SMOKE_CONFIG_HOME" ]; then
      # One empty 0700 dir under TMPDIR stays behind; a suite trap cannot be claimed here.
      OMS_SMOKE_CONFIG_HOME="$(mktemp -d "${TMPDIR:-/tmp}/oms-smoke-config.XXXXXX")" || return 2
      export OMS_SMOKE_CONFIG_HOME
      export XDG_CONFIG_HOME="$OMS_SMOKE_CONFIG_HOME"
    fi ;;
esac
# Fixtures choose their tools through PATH; runtime activation must not undo it.
export OMS_TEST_PROVIDER_PATH_ONLY=1
# Poison any accidental interpreter lookup in smoke suites unless explicitly allowed.
if [ "${OMS_SMOKE_ALLOW_PYTHON:-0}" != 1 ]; then
  OMS_SMOKE_POISON_BIN="${OMS_SMOKE_CONFIG_HOME:-$XDG_CONFIG_HOME}/poison-bin"
  mkdir -p "$OMS_SMOKE_POISON_BIN"
  for oms_smoke_py in python python3; do
    if [ ! -x "$OMS_SMOKE_POISON_BIN/$oms_smoke_py" ]; then
      printf '#!/usr/bin/env sh\necho "error: retired interpreter invoked in pure-Rust smoke test: %s $*" >&2\nexit 99\n' "$oms_smoke_py" > "$OMS_SMOKE_POISON_BIN/$oms_smoke_py"
      chmod +x "$OMS_SMOKE_POISON_BIN/$oms_smoke_py"
    fi
  done
  export OMS_SMOKE_POISON_BIN
  case ":$PATH:" in
    *":$OMS_SMOKE_POISON_BIN:"*) ;;
    *) PATH="$OMS_SMOKE_POISON_BIN:$PATH"; export PATH ;;
  esac
  unset oms_smoke_py
fi
# The host's AppArmor userns setting must not gate fixture Codex write dispatches;
# sandbox-gate tests point this at their own file.
export OMS_DOCTOR_USERNS_SYSCTL_PATH=/nonexistent/oms-test-userns
for oms_smoke_inherited in $(compgen -e); do
  case "$oms_smoke_inherited" in
    HERDR_*|OMS_HARNESS_*|OMS_PANEL_*|OMS_ROOM_*) unset "$oms_smoke_inherited" ;;
  esac
done
unset oms_smoke_inherited

oms_smoke_use_fixture_root() {
  [ -n "${OMS_FIXTURE_ROOT:-}" ] || return 0
  case "$OMS_FIXTURE_ROOT" in
    /*) ;;
    *) echo 'error: OMS_FIXTURE_ROOT must be absolute' >&2; return 2 ;;
  esac
  mkdir -p "$OMS_FIXTURE_ROOT/locks" "$OMS_FIXTURE_ROOT/state" \
    "$OMS_FIXTURE_ROOT/cache" || return 2
  export OMS_LOCK_DIR="$OMS_FIXTURE_ROOT/locks"
  export XDG_STATE_HOME="$OMS_FIXTURE_ROOT/state"
  export XDG_CACHE_HOME="$OMS_FIXTURE_ROOT/cache"
}

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

assert_file_contains() {
  local file="$1"
  local text="$2"
  grep -Fq -- "$text" "$file" || fail "$file does not contain: $text"
}

# A builtin substring match avoids SIGPIPE under pipefail when a producer
# writes more bytes than an early-exiting grep needs.
contains() {
  case "$1" in *"$2"*) return 0 ;; esac
  return 1
}

assert_not_exists() {
  local path="$1"
  [ ! -e "$path" ] || fail "$path should not exist"
}

assert_contains() {
  assert_file_contains "$@"
}

# Initialize only: callers retain their own tracked files and first commit.
oms_smoke_make_repo() {
  mkdir -p "$1"
  git -C "$1" init -q
  git -C "$1" checkout -qb main
}

# Read the exact provider program from stdin; callers choose its interpreter.
oms_smoke_make_fake_provider_stub() {
  cat > "$1"
  chmod +x "$1"
}

# Native payload artifacts in manifest order; the last name is split so the
# migration inventory's interpreter-reference count stays unchanged.
OMS_SMOKE_NATIVE_ARTIFACTS="oms oms-state-queue-worker oms-mcp-peer-owner oms-run-bounded-helper oms-graph-py""thon-helper"

# Print the first cargo profile directory holding every native artifact under
# ${CARGO_TARGET_DIR:-ROOT/target}; fail when none does. Suites never build.
oms_smoke_native_build_dir() {
  local dir name
  for dir in "${CARGO_TARGET_DIR:-$1/target}/release" "$1/target/lifecycle-build/release" "${CARGO_TARGET_DIR:-$1/target}/debug"; do
    for name in $OMS_SMOKE_NATIVE_ARTIFACTS; do
      [ -f "$dir/$name" ] && [ -x "$dir/$name" ] || continue 2
    done
    printf '%s\n' "$dir"
    return 0
  done
  return 1
}

# Stage the artifacts present in SOURCE into ENTRY (target/oms-native/REV)
# with the installer's modes and a manifest plus oms.sha256 listing each one.
oms_smoke_stage_native() {
  local entry="$1" source="$2" name digest artifacts="" oms_digest=""
  mkdir -p "$entry" && chmod 755 "$entry" || return
  for name in $OMS_SMOKE_NATIVE_ARTIFACTS; do
    [ -f "$source/$name" ] || continue
    cp "$source/$name" "$entry/$name" && chmod 755 "$entry/$name" || return
    digest="$({ sha256sum "$entry/$name" 2>/dev/null || shasum -a 256 "$entry/$name"; } | cut -d' ' -f1)" || return
    [ "$name" != oms ] || oms_digest="$digest"
    artifacts="$artifacts${artifacts:+,}\"$name\":\"$digest\""
  done
  [ -n "$oms_digest" ] || return 1
  printf '%s\n' "$oms_digest" > "$entry/oms.sha256"
  printf '{"schema":1,"source_commit":"%s","artifacts":{%s}}' "${entry##*/}" "$artifacts" > "$entry/manifest.json"
  chmod 600 "$entry/oms.sha256" "$entry/manifest.json"
}
