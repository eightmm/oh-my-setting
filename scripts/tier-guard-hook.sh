#!/usr/bin/env bash
set -euo pipefail

# PreToolUse hook on the edit tools: model tiering's one hard edge. Planning
# and review belong to the session model; implementation belongs to a worker
# one price rank below — a subagent, or an OMS delegate. This hook sees an
# edit the session model itself is about to make inside an adopted repo and,
# by OMS_TIER_GUARD, says so once per session (advise, the default), asks the
# operator (ask), or refuses (deny). Edits by a subagent (the payload carries
# agent_id) or by a harness worker (OMS_HARNESS_CHILD=1) pass untouched: they
# are the tier the policy wants writing. Files outside the repo, under .oms/,
# under docs/, and markdown pass too — prose is not implementation. Fail-open
# by construction: it exits 0 on every path, and advise only adds context.

case "${OMS_TIER_GUARD:-advise}" in
  off|0|false|FALSE|no|NO) exit 0 ;;
esac
[ "${OMS_HARNESS_CHILD:-0}" != 1 ] || exit 0

payload="$(cat 2>/dev/null || true)"
[ -n "$payload" ] || exit 0

repo="${OMS_STATE_REPO:-$PWD}"
root="$(git -C "$repo" rev-parse --show-toplevel 2>/dev/null || printf '%s' "$repo")"
[ -d "$root/.oms" ] || exit 0

OMS_TGH_PAYLOAD="$payload" OMS_TGH_ROOT="$root" OMS_TGH_MODE="${OMS_TIER_GUARD:-advise}" \
  python3 - <<'PY' 2>/dev/null || true
import json
import os
import re
import sys

EDIT_TOOLS = {"Edit", "Write", "MultiEdit", "NotebookEdit"}

try:
    row = json.loads(os.environ["OMS_TGH_PAYLOAD"])
except (ValueError, KeyError):
    raise SystemExit(0)
if not isinstance(row, dict) or row.get("tool_name") not in EDIT_TOOLS:
    raise SystemExit(0)
# A subagent is the worker tier: its edits are the ones the policy wants.
if row.get("agent_id") or row.get("agent_type"):
    raise SystemExit(0)
tool_input = row.get("tool_input")
if not isinstance(tool_input, dict):
    raise SystemExit(0)
path = tool_input.get("file_path") or tool_input.get("notebook_path")
if not isinstance(path, str) or not path:
    raise SystemExit(0)
cwd = row.get("cwd") if isinstance(row.get("cwd"), str) and row.get("cwd") else os.getcwd()
if not os.path.isabs(path):
    path = os.path.join(cwd, path)
path = os.path.realpath(path)
root = os.path.realpath(os.environ["OMS_TGH_ROOT"])
if path != root and not path.startswith(root + os.sep):
    raise SystemExit(0)
rel = os.path.relpath(path, root)
if rel.startswith(".oms" + os.sep) or rel.startswith("docs" + os.sep) or rel.lower().endswith(".md"):
    raise SystemExit(0)

mode = os.environ.get("OMS_TGH_MODE", "advise")
reason = (
    "tier guard: %s is implementation. The session model plans and reviews; "
    "a worker one rank below writes (oms peer-delegate, or a subagent). "
    "OMS_TIER_GUARD=off|advise|ask|deny selects how firmly this holds." % rel
)
if mode in ("deny", "ask"):
    json.dump({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": mode,
        "permissionDecisionReason": reason,
    }}, sys.stdout)
    sys.stdout.write("\n")
    raise SystemExit(0)

# advise: once per session, so the reminder is a fact the model has read, not
# a line on every edit.
session = re.sub(r"[^A-Za-z0-9_-]", "", str(row.get("session_id") or ""))[:80] or "session"
marker_dir = os.path.join(root, ".oms", "hooks")
marker = os.path.join(marker_dir, "tier-guard-" + session)
if os.path.exists(marker):
    raise SystemExit(0)
try:
    os.makedirs(marker_dir, exist_ok=True)
    with open(marker, "w", encoding="utf-8"):
        pass
except OSError:
    pass
json.dump({"hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "additionalContext": reason,
}}, sys.stdout)
sys.stdout.write("\n")
PY
exit 0
