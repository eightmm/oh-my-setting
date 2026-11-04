# shellcheck shell=bash
# Fixtures establish their own operator, panel and room identities, even when
# invoked from a managed terminal or a delegated worker.
unset TMUX TMUX_PANE OMS_STATE_REPO OMS_PLAN_LEASE_ID OMS_TASK_ID OMS_ATTEMPT_ID
unset CODEX_THREAD_ID CLAUDE_CODE_SESSION_ID
# An admission or gate verify budget set by the caller is not the fixtures' own.
unset OMS_PEER_VERIFY_TIMEOUT
# Runtime seat-wall and worker-guard knobs of an accepting caller are not the fixtures' own.
unset OMS_PEER_TIMEOUT OMS_PEER_PRINT_TIMEOUT OMS_WORKER_GUARD_STRICT OMS_WORKER_GUARD_OFF
unset NVM_BIN NVM_DIR
# Match CI's hermetic Git configuration. Fixtures that need commits must set
# their own identity; ambient developer identity must never make them pass.
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_NOSYSTEM=1
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
