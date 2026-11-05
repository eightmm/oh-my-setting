# shellcheck shell=bash
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

# Keep transaction fixtures on the working implementation without copying a repo.
oms_smoke_make_update_source_fixture() {
  local source="$1"
  local file
  mkdir -p "$source/scripts/lib"
  cp "$ROOT/scripts/update.sh" "$source/scripts/update.sh"
  for file in install-contract.sh install-lifecycle-lock.sh canonical-sync.sh file-lock.sh poll.sh; do
    cp "$ROOT/scripts/lib/$file" "$source/scripts/lib/$file"
  done
}
