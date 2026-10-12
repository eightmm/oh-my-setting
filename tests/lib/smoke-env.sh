# shellcheck shell=bash
# Fixtures establish their own operator, panel and room identities, even when
# invoked from a managed terminal or a delegated worker.
unset TMUX TMUX_PANE OMS_STATE_REPO OMS_PLAN_LEASE_ID
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
