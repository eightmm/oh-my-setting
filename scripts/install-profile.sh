#!/usr/bin/env bash
set -euo pipefail

# Inspect or install only the locked tools required by selected OMS capability
# profiles. The legacy install-tools.sh full council remains unchanged; this
# script reuses its digest-verified transactional installer functions.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
ACTION=apply
PRIMARY_PROVIDER="${OH_MY_SETTING_PRIMARY_PROVIDER:-auto}"
UPGRADE_FLAG=0
ALLOW_MISSING=0
ALLOW_MISSING_SET=0
REAPPLY=0
RECEIPT="${OMS_CAPABILITY_RECEIPT:-${XDG_CONFIG_HOME:-$HOME/.config}/oh-my-setting/capabilities.json}"
declare -a PROFILES=()
declare -a SELECTED_TOOLS=()

usage() {
  cat <<'USAGE'
Usage: install-profile.sh [--plan|--check|--apply] (--profile NAME... | --reapply) [options]

Resolve the smallest locked OMS tool set needed by one or more capability
profiles. Optional GitHub, Notion, council, research, HPC, container, and remote
adapters no longer have to be installed with the core runtime.

Actions:
  --plan                  Print the typed install plan and change nothing.
  --check                 Check the selected commands/environment only.
  --apply                 Install selected OMS-managed tools (default), check
                          the result, and write a private capability receipt.
  --reapply               Reuse the exact profiles, primary provider, and
                          degraded-mode policy from the existing receipt. This
                          is the update path; combine with --upgrade to refresh.

Options:
  --profile NAME          core, council, github, notion, research, hpc,
                          container, remote, or full. Repeatable.
  --primary-provider NAME auto (default), codex, claude, or agy for core.
  --upgrade               Refresh selected managed tools to tools.lock.json.
  --allow-missing         Permit a degraded apply receipt when external or
                          optional capabilities remain unavailable.
  --receipt PATH          Private user-local receipt path.
  --dry-run               Alias for --plan.
  -h, --help              Show this help.
USAGE
}

fail() { echo "install-profile: $*" >&2; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --plan) ACTION=plan; shift ;;
    --check) ACTION=check; shift ;;
    --apply) ACTION=apply; shift ;;
    --profile)
      [ "$#" -ge 2 ] || fail "--profile requires a value"
      PROFILES+=("$2"); shift 2 ;;
    --reapply) REAPPLY=1; shift ;;
    --primary-provider)
      [ "$#" -ge 2 ] || fail "--primary-provider requires a value"
      PRIMARY_PROVIDER="$2"; shift 2 ;;
    --upgrade) UPGRADE_FLAG=1; shift ;;
    --allow-missing) ALLOW_MISSING=1; ALLOW_MISSING_SET=1; shift ;;
    --receipt)
      [ "$#" -ge 2 ] || fail "--receipt requires a path"
      RECEIPT="$2"; shift 2 ;;
    --dry-run) ACTION=plan; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done
command -v python3 >/dev/null 2>&1 || fail "python3 is required"
if [ "$REAPPLY" -eq 1 ]; then
  [ "${#PROFILES[@]}" -eq 0 ] || fail "--reapply cannot be combined with --profile"
  [ -f "$RECEIPT" ] && [ ! -L "$RECEIPT" ] || fail "--reapply needs a regular capability receipt: $RECEIPT"
  receipt_values="$(python3 - "$RECEIPT" <<'PY_RECEIPT'
import json
import sys
try:
    with open(sys.argv[1], encoding="utf-8") as handle:
        row = json.load(handle)
    plan = row.get("plan")
    if row.get("schema") != 1 or not isinstance(plan, dict):
        raise ValueError
    requested = plan.get("requested")
    primary = plan.get("primary_provider")
    if not isinstance(requested, list) or not requested or not all(isinstance(value, str) and value for value in requested):
        raise ValueError
    if primary not in ("codex", "claude", "agy"):
        raise ValueError
    print(primary)
    print("1" if row.get("allow_missing") is True else "0")
    for value in requested:
        print(value)
except (OSError, ValueError, TypeError, json.JSONDecodeError):
    raise SystemExit(1)
PY_RECEIPT
)" || fail "invalid capability receipt: $RECEIPT"
  PRIMARY_PROVIDER="$(printf '%s\n' "$receipt_values" | sed -n '1p')"
  if [ "$ALLOW_MISSING_SET" -eq 0 ]; then
    ALLOW_MISSING="$(printf '%s\n' "$receipt_values" | sed -n '2p')"
  fi
  while IFS= read -r value; do
    [ -n "$value" ] && PROFILES+=("$value")
  done <<EOF_REAPPLY
$(printf '%s\n' "$receipt_values" | sed -n '3,$p')
EOF_REAPPLY
fi
[ "${#PROFILES[@]}" -gt 0 ] || fail "at least one --profile or --reapply is required"
case "$PRIMARY_PROVIDER" in
  auto)
    if command -v codex >/dev/null 2>&1; then PRIMARY_PROVIDER=codex
    elif command -v claude >/dev/null 2>&1; then PRIMARY_PROVIDER=claude
    elif command -v agy >/dev/null 2>&1; then PRIMARY_PROVIDER=agy
    else PRIMARY_PROVIDER=codex
    fi
    ;;
  codex|claude|agy) ;;
  *) fail "primary provider must be auto, codex, claude, or agy" ;;
esac

runtime=(python3 "$ROOT/scripts/lib/oms_core.py" --repo "$ROOT")
plan_json="$("${runtime[@]}" profile install-plan "${PROFILES[@]}" --primary-provider "$PRIMARY_PROVIDER")" || exit $?
if [ "$ACTION" = plan ]; then
  printf '%s\n' "$plan_json"
  exit 0
fi

check_json() {
  "${runtime[@]}" profile check "${PROFILES[@]}"
}

if [ "$ACTION" = check ]; then
  check_json
  exit $?
fi
[ "$ACTION" = apply ] || fail "invalid action: $ACTION"

while IFS= read -r line; do
  [ -n "$line" ] && SELECTED_TOOLS+=("$line")
done <<EOF_PLAN
$(printf '%s' "$plan_json" | python3 -c '
import json, re, sys
row = json.load(sys.stdin)
for value in row.get("managed_tools", []):
    if not isinstance(value, str) or not re.fullmatch(r"[a-z][a-z0-9-]{0,31}", value):
        raise SystemExit("invalid managed tool in profile plan")
    print(value)
')
EOF_PLAN

if [ "${#SELECTED_TOOLS[@]}" -gt 0 ]; then
  set --
  . "$ROOT/scripts/install-tools.sh"
  UPGRADE="$UPGRADE_FLAG"

  selected() {
    local wanted="$1" item
    for item in "${SELECTED_TOOLS[@]}"; do
      [ "$item" = "$wanted" ] && return 0
    done
    return 1
  }

  needs_node=0
  for item in "${SELECTED_TOOLS[@]}"; do
    case "$item" in node|claude|codex|ntn) needs_node=1 ;; esac
  done
  if [ "$needs_node" -eq 1 ]; then
    ensure_node
    ensure_local_bin_path
    ensure_writable_npm_global
  else
    ensure_local_bin_path
  fi

  for item in claude codex ntn; do
    selected "$item" || continue
    preflight_npm_shim "$item"
  done
  selected uv && ensure_uv
  selected claude && install_npm_global claude
  selected codex && install_npm_global codex
  selected ntn && install_npm_global ntn
  selected agy && install_antigravity
  selected gh && install_gh
  for item in claude codex ntn; do
    selected "$item" || continue
    write_npm_shim "$item"
    verify_resolved_npm_binary "$item"
  done
fi

# The tools this apply just installed live under ~/.local/bin, which future
# shells get from the persisted rc line but the CURRENT process does not have
# yet on hosts whose default PATH omits it (Windows Git Bash). Verifying with
# the old PATH judged a successful install unavailable.
case ":$PATH:" in
  *":$HOME/.local/bin:"*) ;;
  *) export PATH="$HOME/.local/bin:$PATH" ;;
esac

check_rc=0
check_result="$(check_json)" || check_rc=$?
if [ "$check_rc" -ne 0 ] && [ "$ALLOW_MISSING" -ne 1 ]; then
  printf '%s\n' "$check_result"
  echo "install-profile: selected capabilities remain unavailable" >&2
  exit "$check_rc"
fi

receipt_parent="$(dirname "$RECEIPT")"
[ ! -L "$receipt_parent" ] || fail "receipt directory must not be a symbolic link"
umask 077
mkdir -p "$receipt_parent"
[ ! -L "$RECEIPT" ] || fail "receipt must not be a symbolic link"
OMS_CAP_PLAN="$plan_json" OMS_CAP_CHECK="$check_result" OMS_CAP_RECEIPT="$RECEIPT" OMS_CAP_ALLOW_MISSING="$ALLOW_MISSING" python3 <<'PY'
import datetime
import json
import os
import tempfile

path = os.path.abspath(os.environ["OMS_CAP_RECEIPT"])
plan = json.loads(os.environ["OMS_CAP_PLAN"])
row = {
    "schema": 1,
    "updated_at": datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "requested": plan.get("requested", []),
    "primary_provider": plan.get("primary_provider"),
    "plan": plan,
    "check": json.loads(os.environ["OMS_CAP_CHECK"]),
    "allow_missing": os.environ.get("OMS_CAP_ALLOW_MISSING") == "1",
}
parent = os.path.dirname(path)
fd, temp = tempfile.mkstemp(prefix=".capabilities.", dir=parent)
try:
    if hasattr(os, "fchmod"):
        os.fchmod(fd, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as handle:
        json.dump(row, handle, ensure_ascii=False, allow_nan=False, sort_keys=True, indent=2)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(temp, path)
finally:
    try:
        os.unlink(temp)
    except FileNotFoundError:
        pass
print(json.dumps(row, ensure_ascii=False, allow_nan=False, sort_keys=True))
PY

if [ "$check_rc" -ne 0 ]; then
  echo "install-profile: recorded degraded profile (${PROFILES[*]})" >&2
else
  echo "install-profile: ok (${PROFILES[*]})" >&2
fi
