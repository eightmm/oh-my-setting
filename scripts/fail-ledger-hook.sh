#!/usr/bin/env bash
set -euo pipefail

# PostToolUseFailure + PostToolUse hook: give the agent's Bash commands the
# memory the rules always assumed, and take it back when they pass. fail-ledger
# has recorded, warned, and named `oms advise` at the repeat threshold since it
# existed — but only when something called it, which the primary agent
# mid-failure-loop never did. This hook is that call: when a Bash tool run
# fails, it first surfaces what the ledger already knows about this command,
# then records the failure so the second identical attempt crosses the advise
# threshold mechanically. When the same command later exits 0, the hook
# resolves its own row — the ledger's resolved-on-success contract, honored by
# the writer that produced the rows instead of by a human sweep. Fail-open by
# construction: it cannot change the tool result, and it says nothing unless
# the ledger has something to say. When it does, the speech goes to stderr
# with exit 2 — on PostToolUse/PostToolUseFailure that is the one channel
# Claude Code shows to the model without blocking anything; plain stdout on
# these events reaches only the debug log, which is where this hook's context
# went unread until a probe showed the second identical failure arriving with
# no ledger line attached. OMS_FAIL_LEDGER_HOOK=0 disables the whole script;
# OMS_FAIL_LEDGER_RESOLVE=0 disables only the success side.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

case "${OMS_FAIL_LEDGER_HOOK:-1}" in
  0|false|FALSE|no|NO|off|OFF) exit 0 ;;
esac

payload="$(cat 2>/dev/null || true)"
[ -n "$payload" ] || exit 0

[ -x "$ROOT/scripts/fail-ledger.sh" ] || exit 0

# Shell-only precheck: a PostToolUse (success) event only ever resolves an open
# ledger row, so with an absent/empty ledger the python pass below would print
# nothing and touch nothing. It also owns the panel-activity and usage-counter
# side effects, so those environments, and any payload the shell cannot read
# unambiguously, fall through to it unchanged.
fast_idle_success() {
  local ev_re cwd_re base rest top ledger
  case "${OMS_USAGE_TRACK:-0}" in
    0|false|FALSE|no|NO|off|OFF) ;;
    *) return 1 ;;
  esac
  # Mirrors hook_state.record_panel_activity: it acts only for a panel main.
  if [ -n "${OMS_PANEL_MAIN_ATTEMPT:-}" ] && [ "${OMS_HARNESS_CHILD:-}" != 1 ]; then
    case "${OMS_PANEL_SESSION:-}" in oms-*) return 1 ;; esac
  fi
  # A JSON escape can spell a key the shell would read as absent.
  case "$payload" in *'\u'*) return 1 ;; esac
  case "$payload" in *'"PostToolUseFailure"'*) return 1 ;; esac
  ev_re='"hook_event_name"[[:space:]]*:[[:space:]]*"PostToolUse"'
  [[ "$payload" =~ $ev_re ]] || return 1
  rest="${payload#*"${BASH_REMATCH[0]}"}"
  case "$rest" in *'"hook_event_name"'*) return 1 ;; esac
  case "${OMS_FAIL_LEDGER_RESOLVE:-1}" in
    0|false|FALSE|no|NO|off|OFF) return 0 ;;
  esac
  ledger="${OMS_FAIL_LEDGER:-}"
  if [ -z "$ledger" ]; then
    if [ -n "${OMS_STATE_REPO:-}" ]; then
      base="$OMS_STATE_REPO"
    else
      case "$payload" in *'"currentWorkingDirectory"'*) return 1 ;; esac
      cwd_re='"cwd"[[:space:]]*:[[:space:]]*"([^"\\]+)"'
      if [[ "$payload" =~ $cwd_re ]]; then
        base="${BASH_REMATCH[1]}"
        rest="${payload#*"${BASH_REMATCH[0]}"}"
        case "$rest" in *'"cwd"'*) return 1 ;; esac
      elif [[ "$payload" == *'"cwd"'* ]]; then
        return 1
      else
        base="$PWD"
      fi
    fi
    case "$base" in /*|[A-Za-z]:/*) ;; *) return 1 ;; esac
    top="$(git -C "$base" rev-parse --show-toplevel 2>/dev/null)" || return 1
    top="${top//$'\r'/}"
    [ -n "$top" ] || return 1
    ledger="$top/.oms/failures.jsonl"
  fi
  [ ! -s "$ledger" ]
}
if fast_idle_success; then exit 0; fi

# One python3 pass resolves the event repo and decides everything: which side
# of the ledger this event is, and — on the success side — whether the
# fingerprint has an open failure at all. Deliberately not a `check` call:
# resolving needs no git state fingerprint, so this never computes one.
parsed="$(OMS_FLH_PAYLOAD="$payload" \
  OMS_FLH_FAMILIES="$ROOT/scripts/lib/usage-families.json" \
  python3 -X utf8 -I "$ROOT/scripts/lib/cached-main.py" "$ROOT/scripts/lib/fail_ledger_hook.py" 2>/dev/null || true)"
parsed="${parsed//$'\r'/}"
[ -n "$parsed" ] || exit 0
root="$(printf '%s\n' "$parsed" | sed -n 1p)"
mode="$(printf '%s\n' "$parsed" | sed -n 2p)"

if [ "$mode" = "resolve" ]; then
  cmd="$(printf '%s\n' "$parsed" | sed -n 3p)"
  [ -n "$cmd" ] || exit 0
  # Silent on purpose: the resolve receipt is bookkeeping, and a command that
  # just passed is the last thing the agent needs narrated back to it.
  "$ROOT/scripts/fail-ledger.sh" --repo "$root" resolve --cmd "$cmd" \
    --how "same command passed (exit 0)" >/dev/null 2>&1 || true
  exit 0
fi

exit_code="$(printf '%s\n' "$parsed" | sed -n 3p)"
cmd="$(printf '%s\n' "$parsed" | sed -n 4p)"
case "$exit_code" in ''|*[!0-9]*) exit 0 ;; esac
[ -n "$cmd" ] || exit 0

# Known-failure context first: the ledger speaks on stderr, and check exits 3
# exactly when this fingerprint is an unresolved failure in an unchanged repo.
known=""
if ! known="$("$ROOT/scripts/fail-ledger.sh" --repo "$root" check --cmd "$cmd" 2>&1)"; then
  :
else
  known=""
fi

recorded="$("$ROOT/scripts/fail-ledger.sh" --repo "$root" record \
  --cmd "$cmd" --exit "$exit_code" --kind hook 2>&1 || true)"
advise="$(printf '%s\n' "$recorded" | grep -F 'oms advise' || true)"

# Emit only ledger speech, once per line: prior context and the advise-at-
# threshold hint. The routine "recorded <fp>" receipt is bookkeeping, not
# something the agent should read after every failed command.
speech="$(printf '%s\n%s\n' "$known" "$advise" |
  grep '^fail-ledger:' | awk '!seen[$0]++' || true)"
[ -n "$speech" ] || exit 0
printf '%s\n' "$speech" >&2
exit 2
