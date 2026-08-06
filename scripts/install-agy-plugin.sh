#!/usr/bin/env bash
set -euo pipefail

# Package and import the Antigravity plugin: `agy plugin install` copies the
# plugin directory verbatim into ~/.gemini/config/plugins/<name>, so the
# installer bakes this checkout's absolute paths into a generated copy
# instead of shipping placeholders no expansion would fill.
#
# The plugin carries mcpServers, which is how agy sessions reach the
# journal/handoff/fail-ledger state that the prompt-submit hooks give Claude
# and Codex. It carries hooks.json only for an agy binary whose lifecycle
# surfaces have been certified on this machine, never on a guess: Antigravity
# 1.1.9 accepted a hooks.json component but fired no hook event in headless
# (-p) runs, and 1.1.10 advertises that Stop hooks "run at all" now. An
# advertisement is a reason to re-probe, not evidence.
#
# `--probe-surfaces` (oms update --probe-agy-surfaces) answers the question
# mechanically: under a throwaway HOME it installs a fixture plugin, runs one
# bounded headless turn, and checks whether agy executed the PreInvocation and
# Stop handlers, with the documented payload, and enforced a handler timeout.
# The verdict is cached against the binary fingerprint, so a new agy release
# falls back to MCP-only until it is certified again.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
MODE="${OH_MY_SETTING_AGY_PLUGIN:-auto}"
REMOVE=0
PROBE=0
AGY_BIN="${OMS_AGY_BIN:-agy}"
# What this probe checks. A probe that learns to check more invalidates the
# verdicts of the one that checked less, without waiting for the binary to
# change.
PROBE_SCHEMA=1

# shellcheck source=scripts/lib/model-capability.sh
. "$ROOT/scripts/lib/model-capability.sh"

usage() {
  cat <<'EOF'
Usage: install-agy-plugin.sh [--remove] [--probe-surfaces]

Import the oh-my-setting plugin (harness-state MCP server) into Antigravity.
OH_MY_SETTING_AGY_PLUGIN=0 skips, 1 requires agy, auto (default) installs
when agy is on PATH. --remove uninstalls the plugin.

--probe-surfaces certifies this agy binary's lifecycle hook surfaces under a
throwaway HOME and caches the verdict; only a verified verdict lets a later
install generate hooks.json. It installs nothing.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --remove) REMOVE=1; shift ;;
    --probe-surfaces) PROBE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ "$REMOVE" != "1" ] || [ "$PROBE" != "1" ] ||
  fail "--remove and --probe-surfaces cannot be combined"

# ---------------------------------------------------------------------------
# Surface certification cache. Keyed to the binary the way model-capability
# keys its snapshots: a different agy — an upgrade, a reinstall, a test stub —
# is a cache miss and therefore MCP-only, not an inherited yes.
# ---------------------------------------------------------------------------

agy_surface_cache_file() {
  printf '%s/agy-surfaces.env\n' "$(oms_capability_cache_dir)"
}

# verified | unsupported | unverified for the agy on PATH right now. Anything
# the cache cannot answer for THIS binary and THIS probe is unverified, which
# is the MCP-only path.
agy_surface_verdict() {
  local file stored verdict schema
  file="$(agy_surface_cache_file)"
  if [ ! -f "$file" ]; then printf 'unverified\n'; return 0; fi
  stored="$(oms_capability_read_field "$file" binary_key 2>/dev/null || true)"
  schema="$(oms_capability_read_field "$file" probe_schema 2>/dev/null || true)"
  verdict="$(oms_capability_read_field "$file" verdict 2>/dev/null || true)"
  if [ "$stored" != "$(oms_capability_binary_key "$AGY_BIN")" ] ||
    [ "$schema" != "$PROBE_SCHEMA" ]; then
    printf 'unverified\n'
    return 0
  fi
  case "$verdict" in
    verified|unsupported|unverified) printf '%s\n' "$verdict" ;;
    *) printf 'unverified\n' ;;
  esac
}

agy_surface_record() {
  local verdict="$1" reason="$2" schema="$3" pre="$4" stop="$5" shape="$6" cap="$7"
  local dir tmp version
  dir="$(oms_capability_cache_dir)"
  mkdir -p "$dir" 2>/dev/null || return 1
  version="$("$AGY_BIN" --version 2>/dev/null | head -n 1 | tr -cd 'A-Za-z0-9.-' || true)"
  tmp="$(mktemp "$dir/.agy-surfaces.XXXXXX")" || return 1
  {
    printf 'binary_key=%s\n' "$(oms_capability_binary_key "$AGY_BIN")"
    printf 'agy_version=%s\n' "${version:-unknown}"
    printf 'verdict=%s\n' "$verdict"
    printf 'reason=%s\n' "$reason"
    printf 'schema_accepted=%s\n' "$schema"
    printf 'preinvocation_delivered=%s\n' "$pre"
    printf 'stop_delivered=%s\n' "$stop"
    printf 'payload_shape=%s\n' "$shape"
    printf 'handler_timeout_enforced=%s\n' "$cap"
    printf 'probe_schema=%s\n' "$PROBE_SCHEMA"
    printf 'probed_at=%s\n' "$(date +%s)"
  } > "$tmp"
  mv "$tmp" "$(agy_surface_cache_file)"
}

agy_probe_report() {
  local verdict="$1" reason="$2"
  echo "agy-surfaces: $verdict ($reason)"
  case "$verdict" in
    verified)
      echo "agy-surfaces: run \`oms update\` to install the certified hooks.json"
      ;;
    unsupported)
      echo "agy-surfaces: staying MCP-only; this agy does not deliver the surface"
      ;;
    *)
      echo "agy-surfaces: staying MCP-only; nothing was proven either way"
      ;;
  esac
}

# ---------------------------------------------------------------------------
# The probe. Bounded, and confined to its own temporary tree: agy resolves its
# configuration under $HOME and `plugin install` copies into it, so certifying
# against the real HOME would mean mutating the user's live Antigravity
# configuration to find out whether a feature works.
# ---------------------------------------------------------------------------

agy_probe_run() {
  local seconds="$1" dir="$2" log="$3"
  shift 3
  ( cd "$dir" && HOME="$PROBE_HOME" timeout "$seconds" "$@" </dev/null ) \
    > "$log" 2>&1
}

agy_probe_surfaces() {
  local fixture markers rc
  local schema=no pre=no stop=no shape=no cap=no

  if ! command -v "$AGY_BIN" >/dev/null 2>&1; then
    agy_surface_record unverified absent no no no no no || true
    agy_probe_report unverified "agy CLI absent"
    return 0
  fi
  if ! command -v python3 >/dev/null 2>&1 || ! command -v timeout >/dev/null 2>&1; then
    # Refusing to run beats running unbounded here: the probe starts a CLI that
    # falls back to an interactive login prompt when it cannot authenticate.
    agy_surface_record unverified toolchain no no no no no || true
    agy_probe_report unverified "python3 and timeout are required to probe safely"
    return 0
  fi

  PROBE_WORK="$(mktemp -d "${TMPDIR:-/tmp}/oms-agy-probe.XXXXXX")" || {
    agy_probe_report unverified "no writable temporary directory"
    return 0
  }
  trap 'rm -rf "$PROBE_WORK"' EXIT
  PROBE_HOME="$PROBE_WORK/home"
  fixture="$PROBE_WORK/fixture"
  markers="$PROBE_WORK/markers"
  mkdir -p "$PROBE_HOME" "$fixture" "$markers" "$PROBE_WORK/workspace"
  chmod 700 "$PROBE_HOME"

  # The fixture mirrors the plugin this installer would generate: same
  # component layout, same relative command form, same declared timeout. A
  # probe that certified some other arrangement would certify nothing.
  cat > "$fixture/plugin.json" <<'EOF'
{
  "name": "oms-surface-probe",
  "version": "0.0.0",
  "description": "oh-my-setting Antigravity surface certification probe"
}
EOF
  cat > "$fixture/hooks.json" <<'EOF'
{
  "oms-surface-probe": {
    "PreInvocation": [
      {"type": "command", "command": "bash ./probe-hook.sh preinvocation", "timeout": 5}
    ],
    "Stop": [
      {"type": "command", "command": "bash ./probe-hook.sh stop", "timeout": 5},
      {"type": "command", "command": "bash ./probe-slow.sh", "timeout": 1}
    ]
  }
}
EOF
  {
    printf '%s\n' '#!/usr/bin/env bash'
    printf '%s\n' '# Certification handler: record that this event was delivered, keep the'
    printf '%s\n' '# payload for the shape check, and answer in the shape the real adapter'
    printf '%s\n' '# uses, so a rejected result shape shows up as a failed run.'
    printf 'markers=%q\n' "$markers"
    cat <<'EOF'
event="${1:-unknown}"
cat > "$markers/$event.json" 2>/dev/null || true
: > "$markers/$event.fired" 2>/dev/null || true
if [ "$event" = preinvocation ]; then
  printf '%s\n' '{"injectSteps":[{"ephemeralMessage":"oh-my-setting surface probe"}]}'
else
  printf '%s\n' '{}'
fi
exit 0
EOF
  } > "$fixture/probe-hook.sh"
  {
    printf '%s\n' '#!/usr/bin/env bash'
    printf '%s\n' '# A handler that overruns its declared one-second timeout. If agy enforces'
    printf '%s\n' '# the cap, this marker never appears and the turn still finishes.'
    printf 'markers=%q\n' "$markers"
    cat <<'EOF'
sleep 3
: > "$markers/timeout-violated" 2>/dev/null || true
printf '%s\n' '{}'
exit 0
EOF
  } > "$fixture/probe-slow.sh"
  chmod +x "$fixture/probe-hook.sh" "$fixture/probe-slow.sh"

  # 1. Schema acceptance. A hooks.json agy cannot parse is reported as
  #    "skipped (not found)", so "processed" is the discovery signal — and the
  #    only thing validate proves. Delivery is checked by running the CLI.
  rc=0
  agy_probe_run 60 "$PROBE_WORK" "$PROBE_WORK/validate.log" "$AGY_BIN" plugin validate "$fixture" || rc=$?
  if [ "$rc" -ne 0 ]; then
    agy_surface_record unverified validate no no no no no || true
    agy_probe_report unverified "agy plugin validate failed; see nothing installed"
    return 0
  fi
  if grep -Eq 'hooks[[:space:]]*:.*processed' "$PROBE_WORK/validate.log"; then
    schema=yes
  else
    agy_surface_record unsupported schema no no no no no || true
    agy_probe_report unsupported "agy did not accept a plugin hooks.json component"
    return 0
  fi

  rc=0
  agy_probe_run 60 "$PROBE_WORK" "$PROBE_WORK/install.log" "$AGY_BIN" plugin install "$fixture" || rc=$?
  if [ "$rc" -ne 0 ]; then
    agy_surface_record unverified install "$schema" no no no no || true
    agy_probe_report unverified "the fixture plugin would not install under a throwaway HOME"
    return 0
  fi

  # 2. Delivery. One bounded headless turn with the cheapest prompt that still
  #    makes the CLI run its loop.
  rc=0
  agy_probe_run "${OMS_AGY_PROBE_TIMEOUT:-120}" "$PROBE_WORK/workspace" "$PROBE_WORK/run.log" \
    "$AGY_BIN" -p "Reply with the single word: ok" --output-format text || rc=$?
  if [ "$rc" -ne 0 ]; then
    if grep -Eqi 'authentication (required|failed|interrupted)' "$PROBE_WORK/run.log"; then
      # Expected on this path: credentials live under the real HOME, and
      # copying them into a throwaway one to certify a feature is a worse
      # trade than not certifying it.
      agy_surface_record unverified auth "$schema" no no no no || true
      agy_probe_report unverified "agy cannot authenticate under a throwaway HOME"
    else
      agy_surface_record unverified run "$schema" no no no no || true
      agy_probe_report unverified "the headless probe run did not complete (exit $rc)"
    fi
    return 0
  fi

  [ ! -f "$markers/preinvocation.fired" ] || pre=yes
  [ ! -f "$markers/stop.fired" ] || stop=yes
  [ -f "$markers/timeout-violated" ] || cap=yes

  # 3. Payload shape. The adapter reads exactly these fields; a delivered event
  #    whose payload does not carry them is a surface we still cannot use.
  if [ "$pre" = yes ] && [ "$stop" = yes ]; then
    if OMS_AP_PRE="$markers/preinvocation.json" OMS_AP_STOP="$markers/stop.json" \
      python3 - <<'PY'
import json, os, sys


def load(path):
    try:
        with open(path, encoding="utf-8") as handle:
            value = json.load(handle)
    except (OSError, ValueError):
        return None
    return value if isinstance(value, dict) else None


pre = load(os.environ["OMS_AP_PRE"])
stop = load(os.environ["OMS_AP_STOP"])
if pre is None or stop is None:
    sys.exit(1)
for payload in (pre, stop):
    paths = payload.get("workspacePaths")
    if not payload.get("conversationId"):
        sys.exit(1)
    if not isinstance(paths, list) or not paths:
        sys.exit(1)
if "terminationReason" not in stop:
    sys.exit(1)
PY
    then
      shape=yes
    fi
  fi

  if [ "$pre" != yes ]; then
    agy_surface_record unsupported preinvocation "$schema" "$pre" "$stop" "$shape" "$cap" || true
    agy_probe_report unsupported "a completed headless run never delivered PreInvocation"
  elif [ "$stop" != yes ]; then
    agy_surface_record unsupported stop "$schema" "$pre" "$stop" "$shape" "$cap" || true
    agy_probe_report unsupported "a completed headless run never delivered Stop"
  elif [ "$shape" != yes ]; then
    agy_surface_record unsupported payload "$schema" "$pre" "$stop" "$shape" "$cap" || true
    agy_probe_report unsupported "the delivered payloads are missing fields the adapter reads"
  elif [ "$cap" != yes ]; then
    agy_surface_record unsupported handler_timeout "$schema" "$pre" "$stop" "$shape" "$cap" || true
    agy_probe_report unsupported "a handler outlived its declared timeout"
  else
    agy_surface_record verified ok "$schema" "$pre" "$stop" "$shape" "$cap" || true
    agy_probe_report verified "PreInvocation and Stop delivered, payload and timeout as documented"
  fi
  return 0
}

if [ "$PROBE" = "1" ]; then
  PROBE_HOME=""
  agy_probe_surfaces
  exit 0
fi

case "$MODE" in
  0)
    [ "$REMOVE" = "1" ] || { echo "agy-plugin: skipped (OH_MY_SETTING_AGY_PLUGIN=0)"; exit 0; }
    ;;
  1|auto) ;;
  *) fail "OH_MY_SETTING_AGY_PLUGIN must be 0, 1, or auto" ;;
esac

if ! command -v "$AGY_BIN" >/dev/null 2>&1; then
  [ "$MODE" = "1" ] && fail "agy is not installed"
  echo "agy-plugin: note: agy CLI absent; skipped"
  exit 0
fi

if [ "$REMOVE" = "1" ]; then
  "$AGY_BIN" plugin uninstall oh-my-setting >/dev/null 2>&1 || true
  echo "agy-plugin: uninstalled (if present)"
  exit 0
fi

command -v python3 >/dev/null 2>&1 || fail "python3 is required"
[ -f "$ROOT/scripts/oms-mcp-server.py" ] || fail "oms-mcp-server.py not found under $ROOT"
[ -f "$ROOT/plugins/antigravity/plugin.json" ] || fail "plugin template missing"

BUILD="$(mktemp -d "${TMPDIR:-/tmp}/oms-agy-plugin.XXXXXX")"
trap 'rm -rf "$BUILD"' EXIT

VERDICT="$(agy_surface_verdict)"
HOOKS=0
[ "$VERDICT" != verified ] || HOOKS=1

# An agy that lost its certification — an upgrade, a reinstall — must not keep
# firing the hooks the previous binary earned. `plugin install` copies into an
# existing directory, so dropping hooks.json from the build is not enough to
# retract it; the installed copy has to go first. This is the only place that
# reaches into the installed plugin, and only for the two files generated here.
INSTALLED="$HOME/.gemini/config/plugins/oh-my-setting"
if [ "$HOOKS" != "1" ] && [ -f "$INSTALLED/hooks.json" ]; then
  "$AGY_BIN" plugin uninstall oh-my-setting >/dev/null 2>&1 || true
  rm -f "$INSTALLED/hooks.json" "$INSTALLED/agy-hook.sh" 2>/dev/null || true
  echo "agy-plugin: retracted hooks from an agy that is no longer certified"
fi

OMS_AP_ROOT="$ROOT" OMS_AP_BUILD="$BUILD" OMS_AP_HOOKS="$HOOKS" python3 - <<'PY'
import json, os

root = os.environ["OMS_AP_ROOT"]
build = os.environ["OMS_AP_BUILD"]
with open(os.path.join(root, "plugins/antigravity/plugin.json"), encoding="utf-8") as fh:
    plugin = json.load(fh)
try:
    with open(os.path.join(root, "VERSION"), encoding="utf-8") as fh:
        plugin["version"] = fh.read().strip() or plugin["version"]
except OSError:
    pass
with open(os.path.join(build, "plugin.json"), "w", encoding="utf-8") as fh:
    json.dump(plugin, fh, ensure_ascii=False, indent=2)
    fh.write("\n")
mcp = {
    "mcpServers": {
        "oh-my-setting": {
            "command": "python3",
            "args": [os.path.join(root, "scripts", "oms-mcp-server.py")],
        }
    }
}
with open(os.path.join(build, "mcp_config.json"), "w", encoding="utf-8") as fh:
    json.dump(mcp, fh, ensure_ascii=False, indent=2)
    fh.write("\n")
if os.environ.get("OMS_AP_HOOKS") == "1":
    # Commands resolve against the directory holding hooks.json, which is the
    # installed plugin directory — the adapter therefore travels with the
    # plugin instead of pointing at a path this checkout guesses agy will use.
    # Only the two surfaces the probe certified are wired; the status line
    # stays out until its own payload and merge behaviour are executable-tested.
    hooks = {
        "oh-my-setting": {
            "PreInvocation": [
                {
                    "type": "command",
                    "command": "bash ./agy-hook.sh preinvocation",
                    "timeout": 5,
                }
            ],
            "Stop": [
                {"type": "command", "command": "bash ./agy-hook.sh stop", "timeout": 5}
            ],
        }
    }
    with open(os.path.join(build, "hooks.json"), "w", encoding="utf-8") as fh:
        json.dump(hooks, fh, ensure_ascii=False, indent=2)
        fh.write("\n")
PY

if [ "$HOOKS" = "1" ]; then
  {
    cat <<'ADAPTER_HEAD'
#!/usr/bin/env bash
# Generated by oh-my-setting install-agy-plugin.sh. Do not edit: a reinstall
# overwrites it, and it only exists while this agy binary is certified.
#
# Antigravity lifecycle adapter. agy sends camelCase protojson naming a
# workspace; the harness hooks read Claude-shaped keys and resolve state from
# cwd, so the translation lives here rather than teaching every hook a second
# dialect. Every path prints a JSON object and exits 0 — this runs in front of
# the agent loop, so an adapter that errors is worse than one that says
# nothing.
set -u
ADAPTER_HEAD
    printf 'ROOT=%q\n' "$ROOT"
    cat <<'ADAPTER_BODY'
EVENT="${1:-}"

emit() { printf '%s\n' "$1"; exit 0; }

# Kill switches before any work: an operator who turned this surface off must
# not pay for a python start, and a harness child must never write the primary
# session state.
[ "${OH_MY_SETTING_AGY_HOOKS:-1}" != 0 ] || emit '{}'
[ "${OMS_HARNESS_CHILD:-0}" != 1 ] || emit '{}'
command -v python3 >/dev/null 2>&1 || emit '{}'
[ -d "$ROOT/scripts" ] || emit '{}'

PAYLOAD="$(cat 2>/dev/null || true)"
TMP="$(mktemp "${TMPDIR:-/tmp}/oms-agy-hook.XXXXXX" 2>/dev/null)" || emit '{}'
trap 'rm -f "$TMP"' EXIT

PLAN="$(printf '%s' "$PAYLOAD" | OMS_AGY_EVENT="$EVENT" OMS_AGY_OUT="$TMP" python3 -c '
import json, os, sys

event = os.environ.get("OMS_AGY_EVENT", "")
try:
    payload = json.loads(sys.stdin.read() or "{}")
except ValueError:
    payload = {}
if not isinstance(payload, dict):
    payload = {}
paths = payload.get("workspacePaths") or []
repo = str(paths[0]) if isinstance(paths, list) and paths else ""
conversation = str(payload.get("conversationId") or "")
if event == "preinvocation":
    number = payload.get("invocationNum")
else:
    number = payload.get("executionNum")
try:
    number = int(number)
except (TypeError, ValueError):
    number = 0
adapted = {
    "cwd": repo,
    "session_id": conversation or "nosession",
    "turn_id": "%s:%s" % (conversation, number),
    "hook_event_name": "PreInvocation" if event == "preinvocation" else "Stop",
    "model": str(payload.get("modelName") or ""),
    "prompt": "",
}
# PreInvocation fires before every model call and carries no user prompt, so
# trigger routing has nothing to match on; what remains is the once-a-turn
# state, journal, and CI tick. Gating on the first invocation keeps that from
# running before every step. A payload without the counter counts as the first
# one, which errs toward running less often, never more.
decision = "skip"
if repo and (event != "preinvocation" or number <= 1):
    decision = "run"
with open(os.environ["OMS_AGY_OUT"], "w", encoding="utf-8") as handle:
    json.dump(adapted, handle)
print(decision)
print(repo)
' 2>/dev/null || true)"

DECISION=""
REPO=""
{ IFS= read -r DECISION || true; IFS= read -r REPO || true; } <<PLAN_EOF
$PLAN
PLAN_EOF

[ "$DECISION" = run ] || emit '{}'
[ -n "$REPO" ] && [ -d "$REPO" ] || emit '{}'

case "$EVENT" in
  preinvocation)
    [ "${OMS_SKILL_ROUTER_OFF:-0}" != 1 ] || emit '{}'
    HINT="$( (cd "$REPO" && OMS_STATE_REPO="$REPO" \
      bash "$ROOT/scripts/skill-router.sh" < "$TMP") 2>/dev/null || true)"
    [ -n "$HINT" ] || emit '{}'
    printf '%s' "$HINT" | python3 -c '
import json, sys

text = sys.stdin.read().strip()
print(json.dumps({"injectSteps": [{"ephemeralMessage": text}]}) if text else "{}")
' 2>/dev/null || printf '%s\n' '{}'
    exit 0
    ;;
  stop)
    # Advisory only. The turn guard runs for its journal finish boundary and CI
    # tick with its blocking verdict switched off, and no decision is returned:
    # this surface was certified to deliver Stop, not trusted to stop the
    # primary agent.
    (cd "$REPO" && OMS_STATE_REPO="$REPO" OMS_TURN_GUARD_OFF=1 \
      bash "$ROOT/scripts/turn-guard.sh" < "$TMP") >/dev/null 2>&1 || true
    (cd "$REPO" && OMS_STATE_REPO="$REPO" \
      bash "$ROOT/scripts/telemetry-hook.sh" < "$TMP") >/dev/null 2>&1 || true
    emit '{}'
    ;;
esac
emit '{}'
ADAPTER_BODY
  } > "$BUILD/agy-hook.sh"
  chmod +x "$BUILD/agy-hook.sh"
fi

if out="$("$AGY_BIN" plugin install "$BUILD" 2>&1)"; then
  if [ "$HOOKS" = "1" ]; then
    # hooks.json without the adapter beside it would hand agy a command that
    # cannot run before every model call. Half an install is worse than none.
    if [ -f "$INSTALLED/hooks.json" ] && [ ! -f "$INSTALLED/agy-hook.sh" ]; then
      rm -f "$INSTALLED/hooks.json" 2>/dev/null || true
      echo "agy-plugin: installed oh-my-setting (harness-state MCP server)"
      echo "agy-plugin: warn: the hook adapter did not land; hooks withdrawn" >&2
      exit 0
    fi
    echo "agy-plugin: installed oh-my-setting (MCP server + certified PreInvocation/Stop hooks)"
  else
    echo "agy-plugin: installed oh-my-setting (harness-state MCP server)"
    case "$VERDICT" in
      unsupported)
        echo "agy-plugin: note: hooks omitted; this agy failed surface certification"
        ;;
      *)
        echo "agy-plugin: note: hooks omitted; run \`oms update --probe-agy-surfaces\` to certify"
        ;;
    esac
  fi
else
  echo "warn: agy plugin install failed:" >&2
  printf '%s\n' "$out" >&2
  [ "$MODE" = "1" ] && exit 1
  exit 0
fi
exit 0
