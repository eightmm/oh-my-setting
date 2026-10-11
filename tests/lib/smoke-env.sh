# shellcheck shell=bash
# Fixtures establish their own operator, panel and room identities, even when
# invoked from a managed terminal or a delegated worker.
unset TMUX TMUX_PANE OMS_HARNESS_CHILD OMS_HARNESS_DELEGATE_DEPTH \
  OMS_STATE_REPO OMS_PLAN_LEASE_ID
for oms_smoke_inherited in $(compgen -e); do
  case "$oms_smoke_inherited" in
    HERDR_*|OMS_PANEL_*|OMS_ROOM_*) unset "$oms_smoke_inherited" ;;
  esac
done
unset oms_smoke_inherited
