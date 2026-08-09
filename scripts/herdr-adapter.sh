#!/usr/bin/env bash
set -euo pipefail

# A bounded control-plane adapter for Herdr. It creates layout and controls
# already-recognized agents, but owns no OMS approval, task lease, patch
# admission, landing, commit, or push authority.

ACTION=""
HERDR_BIN="${OMS_HERDR_BIN:-herdr}"
AS_JSON=0
CWD=""
LABEL=""
PANE=""
DIRECTION=""
NAME=""
KIND=""
TARGET=""
PROMPT=""
TIMEOUT_MS=""
READ_SOURCE="recent-unwrapped"
READ_LINES="80"
UNTIL_STATES=()
AGENT_ARGS=()

usage() {
  cat <<'EOF'
Usage: herdr-adapter.sh <command> [options]

Commands:
  check             Feature-detect the complete supported Herdr CLI contract.
  workspace-create  Create a no-focus workspace and validate its JSON result.
  pane-split        Split a pane without changing focus; validate JSON result.
  agent-start       Start a recognized agent with a mandatory bounded wait.
  agent-prompt      Prompt and wait with a mandatory timeout.
  agent-wait        Wait for an exact lifecycle state with a mandatory timeout.
  agent-read        Read bounded recent agent output.
  agent-status      Return the current agent JSON from `herdr agent get`.

Options:
  --herdr-bin PATH  Herdr executable. Default: OMS_HERDR_BIN or herdr.
  --cwd PATH        Existing workspace directory.
  --label TEXT      Workspace label.
  --pane ID         Pane to split or start in.
  --direction DIR   right or down.
  --name NAME       Live agent name for start.
  --kind KIND       Herdr-supported agent kind for start.
  --target TARGET   Live agent name or pane id.
  --prompt TEXT     Prompt submitted atomically by Herdr.
  --timeout-ms N    Explicit wait timeout in milliseconds.
  --until STATE     idle, done, blocked, working, or unknown; repeatable.
  --source SOURCE   visible, recent, or recent-unwrapped.
  --lines N         Number of rendered lines to read. Default: 80.
  --json            Machine-readable output for check.
  -- ARGS...        Arguments passed unchanged after `herdr agent start --`.

Defaults: agent-start 30000 ms; agent-prompt and agent-wait 120000 ms.
Every operation that waits passes an explicit Herdr --timeout. This adapter
never consumes approvals and never invokes OMS patch admission or landing.
Herdr idle/done/blocked values are frontend observations, not verification or
task success. A delegated OMS child may check/read/status/wait, but cannot use
this wrapper to create layout, start another agent, or submit another prompt.
EOF
}

fail() { echo "herdr-adapter: $*" >&2; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    check|workspace-create|pane-split|agent-start|agent-prompt|agent-wait|agent-read|agent-status)
      [ -z "$ACTION" ] || fail "multiple commands: $ACTION, $1"
      ACTION="$1"; shift ;;
    --herdr-bin) [ "$#" -ge 2 ] || fail "--herdr-bin requires a path"; HERDR_BIN="$2"; shift 2 ;;
    --cwd) [ "$#" -ge 2 ] || fail "--cwd requires a path"; CWD="$2"; shift 2 ;;
    --label) [ "$#" -ge 2 ] || fail "--label requires text"; LABEL="$2"; shift 2 ;;
    --pane) [ "$#" -ge 2 ] || fail "--pane requires an id"; PANE="$2"; shift 2 ;;
    --direction) [ "$#" -ge 2 ] || fail "--direction requires right or down"; DIRECTION="$2"; shift 2 ;;
    --name) [ "$#" -ge 2 ] || fail "--name requires a value"; NAME="$2"; shift 2 ;;
    --kind) [ "$#" -ge 2 ] || fail "--kind requires a value"; KIND="$2"; shift 2 ;;
    --target) [ "$#" -ge 2 ] || fail "--target requires a value"; TARGET="$2"; shift 2 ;;
    --prompt) [ "$#" -ge 2 ] || fail "--prompt requires text"; PROMPT="$2"; shift 2 ;;
    --timeout-ms) [ "$#" -ge 2 ] || fail "--timeout-ms requires an integer"; TIMEOUT_MS="$2"; shift 2 ;;
    --until) [ "$#" -ge 2 ] || fail "--until requires a state"; UNTIL_STATES+=("$2"); shift 2 ;;
    --source) [ "$#" -ge 2 ] || fail "--source requires a value"; READ_SOURCE="$2"; shift 2 ;;
    --lines) [ "$#" -ge 2 ] || fail "--lines requires an integer"; READ_LINES="$2"; shift 2 ;;
    --json) AS_JSON=1; shift ;;
    --)
      shift
      while [ "$#" -gt 0 ]; do AGENT_ARGS+=("$1"); shift; done
      ;;
    -h|--help) usage; exit 0 ;;
    *)
      if [ -z "$ACTION" ]; then
        fail "unknown command: $1"
      fi
      fail "unknown argument: $1"
      ;;
  esac
done

[ -n "$ACTION" ] || { usage >&2; exit 2; }
command -v python3 >/dev/null 2>&1 || fail "python3 is required"
case "$HERDR_BIN" in
  */*) [ -x "$HERDR_BIN" ] || fail "Herdr executable not found: $HERDR_BIN" ;;
  *) command -v "$HERDR_BIN" >/dev/null 2>&1 ||
       fail "Herdr executable not found: $HERDR_BIN (install Herdr separately, then retry)" ;;
esac

# Match peer-delegate's no-recursive-delegation contract. Read-only observation
# remains useful to a delegated worker; layout creation, agent launch, and a new
# prompt create another independently costly flow. This is a policy and budget
# boundary only—the host process is still trusted-local, not sandboxed.
if [ "${OMS_HARNESS_CHILD:-0}" = 1 ]; then
  case "$ACTION" in
    workspace-create|pane-split|agent-start|agent-prompt)
      fail "delegated child cannot start or mutate a Herdr flow"
      ;;
  esac
fi

# Herdr's server owns pane process environments, but the CLI process still must
# not inherit a caller's OMS mutation capabilities. This is defense in depth;
# the real authority boundary remains that no approval/landing operation exists
# in this adapter and no lease is accepted as input.
herdr_run() (
  unset OMS_PLAN_LEASE_ID OMS_LEASE_ID OMS_HARNESS_CHILD
  unset OMS_APPROVAL_ID OMS_LANDING_ID
  export OMS_HERDR_ADAPTER=1
  export OMS_HERDR_APPROVAL_AUTHORITY=0
  export OMS_HERDR_LANDING_AUTHORITY=0
  "$HERDR_BIN" "$@"
)

FEATURE_HELP=""
probe_api_schema() {
  local schema
  if ! schema="$(herdr_run api schema --json 2>/dev/null)"; then
    fail "missing required feature 'api_schema': Herdr does not support 'api schema --json'"
  fi
  printf '%s\n' "$schema" | python3 -c '
import json, sys
value = json.load(sys.stdin)
if not isinstance(value, dict) or not value:
    raise SystemExit(1)
' || fail "missing required feature 'api_schema': Herdr returned invalid JSON Schema"
}

probe_help() {
  local feature="$1"
  shift
  if ! FEATURE_HELP="$(herdr_run "$@" --help 2>&1)"; then
    fail "missing required feature '$feature': Herdr does not support '$* --help'"
  fi
}

require_help_fragment() {
  local feature="$1"
  local fragment="$2"
  case "$FEATURE_HELP" in
    *"$fragment"*) ;;
    *) fail "missing required feature '$feature': expected $fragment in Herdr help" ;;
  esac
}

probe_workspace_create() {
  probe_help workspace_create workspace create
  require_help_fragment workspace_create --cwd
  require_help_fragment workspace_create --label
  require_help_fragment workspace_create --no-focus
}

probe_pane_split() {
  probe_help pane_split pane split
  require_help_fragment pane_split --direction
  require_help_fragment pane_split --cwd
  require_help_fragment pane_split --no-focus
}

probe_agent_start() {
  probe_help agent_start agent start
  require_help_fragment agent_start --kind
  require_help_fragment agent_start --pane
  require_help_fragment agent_start --timeout
}

probe_agent_prompt() {
  probe_help agent_prompt agent prompt
  require_help_fragment agent_prompt --wait
  require_help_fragment agent_prompt --timeout
}

probe_agent_wait() {
  probe_help agent_wait agent wait
  require_help_fragment agent_wait --until
  require_help_fragment agent_wait --timeout
}

probe_agent_read() {
  probe_help agent_read agent read
  require_help_fragment agent_read --source
  require_help_fragment agent_read --lines
}

probe_agent_status() {
  probe_help agent_status agent get
}

validate_timeout() {
  case "$1" in *[!0-9]*|"") fail "--timeout-ms must be an integer" ;; esac
  if [ "$1" -le 3000 ] || [ "$1" -gt 300000 ]; then
    fail "--timeout-ms must be greater than 3000 and at most 300000"
  fi
}

validate_target() {
  [ -n "$1" ] || fail "$2 is required"
  case "$1" in *$'\n'*|*$'\r'*) fail "$2 must be one line" ;; esac
}

validate_json_result() {
  local kind="$1"
  if ! python3 -c '
import json, sys
d = json.load(sys.stdin)
kind = sys.argv[1]
result = d.get("result")
if not isinstance(result, dict):
    raise SystemExit(1)
if kind == "workspace":
    valid = (result.get("workspace", {}).get("workspace_id") and
             result.get("tab", {}).get("tab_id") and
             result.get("root_pane", {}).get("pane_id"))
elif kind == "pane":
    valid = result.get("pane", {}).get("pane_id")
elif kind == "agent":
    valid = isinstance(result.get("agent"), dict) and result["agent"]
else:
    valid = False
if not valid:
    raise SystemExit(1)
' "$kind"; then
    fail "Herdr returned invalid $kind JSON"
  fi
}

run_json_command() {
  local kind="$1"
  local output
  local rc
  shift
  if output="$(herdr_run "$@")"; then
    :
  else
    rc=$?
    return "$rc"
  fi
  printf '%s\n' "$output" | validate_json_result "$kind"
  printf '%s\n' "$output" | python3 -c '
import json, sys
d = json.load(sys.stdin)
d["frontend_authority"] = "none"
d["managed_by_oms"] = False
d["success_authority"] = False
d["state_semantics"] = "presence_only"
json.dump(d, sys.stdout, separators=(",", ":"), sort_keys=True)
sys.stdout.write("\n")
'
}

if [ "$ACTION" = check ]; then
  probe_api_schema
  probe_workspace_create
  probe_pane_split
  probe_agent_start
  probe_agent_prompt
  probe_agent_wait
  probe_agent_read
  probe_agent_status
  if [ "$AS_JSON" -eq 1 ]; then
    printf '%s\n' '{"adapter":"herdr","authority":{"approval":false,"landing":false},"contract_probe":"api_schema","features":["workspace_create","pane_split","agent_start","agent_prompt","agent_wait","agent_read","agent_status"],"frontend_authority":"none","managed_by_oms":false,"native_session_identity":"conditional","ok":true,"schema":1,"state_semantics":"presence_only","success_authority":false}'
  else
    printf 'herdr-adapter: required CLI features are available; frontend state is observational; approval, success, and landing authority: none\n'
  fi
  exit 0
fi

if [ "$ACTION" = workspace-create ]; then
  [ -n "$CWD" ] || fail "workspace-create requires --cwd"
  [ -n "$LABEL" ] || fail "workspace-create requires --label"
  [ -d "$CWD" ] || fail "workspace directory does not exist: $CWD"
  CWD="$(cd "$CWD" && pwd -P)"
  case "$CWD" in *$'\r') CWD="${CWD%$'\r'}" ;; esac
  probe_workspace_create
  run_json_command workspace workspace create --cwd "$CWD" --label "$LABEL" --no-focus
  exit $?
fi

if [ "$ACTION" = pane-split ]; then
  validate_target "$PANE" "pane-split --pane"
  case "$DIRECTION" in right|down) ;; *) fail "pane-split --direction must be right or down" ;; esac
  if [ -n "$CWD" ]; then
    [ -d "$CWD" ] || fail "pane directory does not exist: $CWD"
    CWD="$(cd "$CWD" && pwd -P)"
    case "$CWD" in *$'\r') CWD="${CWD%$'\r'}" ;; esac
  fi
  probe_pane_split
  pane_cmd=(pane split "$PANE" --direction "$DIRECTION")
  [ -z "$CWD" ] || pane_cmd+=(--cwd "$CWD")
  pane_cmd+=(--no-focus)
  run_json_command pane "${pane_cmd[@]}"
  exit $?
fi

if [ "$ACTION" = agent-start ]; then
  validate_target "$NAME" "agent-start --name"
  case "$NAME" in
    [a-z]*) ;;
    *) fail "agent-start --name must match [a-z][a-z0-9_-]{0,31}" ;;
  esac
  case "$NAME" in *[!a-z0-9_-]*) fail "agent-start --name must match [a-z][a-z0-9_-]{0,31}" ;; esac
  [ "${#NAME}" -le 32 ] || fail "agent-start --name must match [a-z][a-z0-9_-]{0,31}"
  validate_target "$KIND" "agent-start --kind"
  case "$KIND" in *[!A-Za-z0-9_-]*) fail "agent-start --kind contains unsafe characters" ;; esac
  validate_target "$PANE" "agent-start --pane"
  [ -n "$TIMEOUT_MS" ] || TIMEOUT_MS="${OMS_HERDR_START_TIMEOUT_MS:-30000}"
  validate_timeout "$TIMEOUT_MS"
  probe_agent_start
  start_cmd=(agent start "$NAME" --kind "$KIND" --pane "$PANE" --timeout "$TIMEOUT_MS" --)
  # Bash 3.2 treats an empty array expansion as unbound under `set -u`.
  if [ "${#AGENT_ARGS[@]}" -gt 0 ]; then
    start_cmd+=("${AGENT_ARGS[@]}")
  fi
  run_json_command agent "${start_cmd[@]}"
  exit $?
fi

if [ "$ACTION" = agent-prompt ]; then
  validate_target "$TARGET" "agent-prompt --target"
  [ -n "$PROMPT" ] || fail "agent-prompt requires --prompt"
  [ -n "$TIMEOUT_MS" ] || TIMEOUT_MS="${OMS_HERDR_WAIT_TIMEOUT_MS:-120000}"
  validate_timeout "$TIMEOUT_MS"
  probe_agent_prompt
  prompt_cmd=(agent prompt "$TARGET" "$PROMPT" --wait --timeout "$TIMEOUT_MS")
  if [ "${#UNTIL_STATES[@]}" -gt 0 ]; then
    for state in "${UNTIL_STATES[@]}"; do
      case "$state" in idle|done|blocked|working|unknown) ;; *) fail "unsupported --until state: $state" ;; esac
      prompt_cmd+=(--until "$state")
    done
  fi
  run_json_command agent "${prompt_cmd[@]}"
  exit $?
fi

if [ "$ACTION" = agent-wait ]; then
  validate_target "$TARGET" "agent-wait --target"
  [ "${#UNTIL_STATES[@]}" -gt 0 ] || fail "agent-wait requires at least one --until state"
  [ -n "$TIMEOUT_MS" ] || TIMEOUT_MS="${OMS_HERDR_WAIT_TIMEOUT_MS:-120000}"
  validate_timeout "$TIMEOUT_MS"
  probe_agent_wait
  wait_cmd=(agent wait "$TARGET")
  for state in "${UNTIL_STATES[@]}"; do
    case "$state" in idle|done|blocked|working|unknown) ;; *) fail "unsupported --until state: $state" ;; esac
    wait_cmd+=(--until "$state")
  done
  wait_cmd+=(--timeout "$TIMEOUT_MS")
  run_json_command agent "${wait_cmd[@]}"
  exit $?
fi

if [ "$ACTION" = agent-read ]; then
  validate_target "$TARGET" "agent-read --target"
  case "$READ_SOURCE" in visible|recent|recent-unwrapped) ;; *) fail "unsupported --source: $READ_SOURCE" ;; esac
  case "$READ_LINES" in *[!0-9]*|"") fail "--lines must be a positive integer" ;; esac
  if [ "$READ_LINES" -le 0 ] || [ "$READ_LINES" -gt 10000 ]; then
    fail "--lines must be between 1 and 10000"
  fi
  probe_agent_read
  herdr_run agent read "$TARGET" --source "$READ_SOURCE" --lines "$READ_LINES"
  exit $?
fi

if [ "$ACTION" = agent-status ]; then
  validate_target "$TARGET" "agent-status --target"
  probe_agent_status
  run_json_command agent agent get "$TARGET"
  exit $?
fi

fail "unknown command: $ACTION"
