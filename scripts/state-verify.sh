#!/usr/bin/env bash
set -euo pipefail

# Read-only consistency verdict over a repository's .oms harness state.
# Shared state is the harness spine: a dangling pointer, a contradictory task
# packet, or a stale derived view is inherited as false confidence by every
# CLI that reads it, and until now each validator judged only its own family.
# This composes the existing engines — oms-run validate (JSONL schemas, plan
# references), artifact-index validate (artifact lineage), agent-task status,
# journal status — and adds the cross-family checks none of them owns:
# CURRENT pointers that name missing threads, active packets with a close
# timestamp, unparseable packet timestamps, orphaned delegation markers, lock
# files inside .oms, and derived journal views behind their events. It never
# repairs anything; every finding names the command that would.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"

usage() {
  cat <<'EOF'
Usage: state-verify.sh [--repo PATH] [--json]

Verify a repository's .oms harness state: delegate to the per-family
validators, then cross-check references between families. Read-only; every
finding names its remedy command.

Options:
  --repo PATH  Repository to verify. Default: current directory.
  --json       Emit one JSON object instead of text.
  -h, --help   Show this help.

Exit status: 0 clean, 1 at least one finding, 2 misuse.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

REPO="$PWD"
AS_JSON=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --json) AS_JSON=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

command -v python3 >/dev/null 2>&1 || fail "python3 is required"
REPO="$(oms_repo_root "$REPO")"

if [ ! -d "$REPO/.oms" ]; then
  if [ "$AS_JSON" = "1" ]; then
    printf '{"schema": 1, "repo": "%s", "adopted": false, "findings": []}\n' "$REPO"
  else
    echo "state-verify: $REPO is not harness-adopted (no .oms); nothing to verify"
  fi
  exit 0
fi

TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-state-verify.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

# Delegated engines. Their exit codes are data here, not verdicts: this
# script re-reports their findings under one summary.
RUN_VALIDATE_EXIT=0
"$ROOT/scripts/oms-run.sh" validate --dir "$REPO/.oms" \
  > "$TMP/run-validate.out" 2>&1 || RUN_VALIDATE_EXIT=$?
ARTIFACT_EXIT=0
if [ -f "$REPO/.oms/artifacts/index.jsonl" ]; then
  "$ROOT/scripts/artifact-index.sh" --repo "$REPO" validate \
    > "$TMP/artifact.out" 2>&1 || ARTIFACT_EXIT=$?
else
  : > "$TMP/artifact.out"
fi
LIFECYCLE_EXIT=0
if [ -f "$REPO/.oms/lifecycle/events.jsonl" ]; then
  "$ROOT/scripts/agent-events.sh" --repo "$REPO" validate \
    > "$TMP/lifecycle.out" 2>&1 || LIFECYCLE_EXIT=$?
else
  : > "$TMP/lifecycle.out"
fi
APPROVAL_EXIT=0
"$ROOT/scripts/approval-inbox.sh" --repo "$REPO" validate \
  > "$TMP/approval.out" 2>&1 || APPROVAL_EXIT=$?
"$ROOT/scripts/agent-task.sh" --repo "$REPO" status --json \
  > "$TMP/task.json" 2>/dev/null || printf '{}' > "$TMP/task.json"
"$ROOT/scripts/journal.sh" status --repo "$REPO" --json \
  > "$TMP/journal.json" 2>/dev/null || printf '{}' > "$TMP/journal.json"

OMS_SV_REPO="$REPO" OMS_SV_TMP="$TMP" OMS_SV_JSON="$AS_JSON" \
OMS_SV_RUN_EXIT="$RUN_VALIDATE_EXIT" OMS_SV_ARTIFACT_EXIT="$ARTIFACT_EXIT" \
OMS_SV_LIFECYCLE_EXIT="$LIFECYCLE_EXIT" OMS_SV_APPROVAL_EXIT="$APPROVAL_EXIT" \
python3 <<'PY'
import glob
import json
import os
import re
import sys

repo = os.environ["OMS_SV_REPO"]
tmp = os.environ["OMS_SV_TMP"]
as_json = os.environ["OMS_SV_JSON"] == "1"
oms = os.path.join(repo, ".oms")
findings = []

def finding(level, family, message, remedy):
    findings.append(
        {"level": level, "family": family, "message": message, "remedy": remedy}
    )

def load_json(name):
    try:
        with open(os.path.join(tmp, name)) as handle:
            return json.load(handle)
    except Exception:
        return {}

# --- delegated: oms-run validate (JSONL schemas, enums, plan references) ---
run_exit = int(os.environ["OMS_SV_RUN_EXIT"])
if run_exit != 0:
    detail = ""
    try:
        with open(os.path.join(tmp, "run-validate.out")) as handle:
            bad = [l.strip() for l in handle if not l.startswith("ok")]
        detail = "; ".join(bad[:3])
    except OSError:
        pass
    finding("fail", "jsonl", "oms-run validate found invalid rows or schema drift: %s" % detail,
            "oms run validate --dir %s/.oms" % repo)

# --- delegated: artifact-index validate (errors fail, stale refs warn) ---
artifact_exit = int(os.environ["OMS_SV_ARTIFACT_EXIT"])
if artifact_exit == 2:
    finding("fail", "artifacts", "artifact-index validate could not run",
            "oms artifact-index --repo %s validate" % repo)
elif artifact_exit != 0:
    bad = stale = 0
    try:
        with open(os.path.join(tmp, "artifact.out")) as handle:
            for line in handle:
                if line.startswith("BAD"):
                    bad += 1
                elif line.startswith("STALE"):
                    stale += 1
    except OSError:
        pass
    if bad:
        finding("fail", "artifacts", "%d invalid artifact index row(s)" % bad,
                "oms artifact-index --repo %s validate" % repo)
    if stale:
        finding("warn", "artifacts", "%d artifact reference(s) point at missing files" % stale,
                "oms artifact-index --repo %s prune --stale" % repo)
    if not bad and not stale:
        finding("fail", "artifacts", "artifact-index validate failed",
                "oms artifact-index --repo %s validate" % repo)

# --- delegated: durable worker lifecycle and private approval CAS -----------
lifecycle_exit = int(os.environ["OMS_SV_LIFECYCLE_EXIT"])
if lifecycle_exit != 0:
    finding("fail", "lifecycle", "agent lifecycle stream is invalid",
            "oms agent-events --repo %s validate" % repo)
approval_exit = int(os.environ["OMS_SV_APPROVAL_EXIT"])
if approval_exit != 0:
    finding("fail", "approvals", "private approval stream or its permissions are invalid",
            "oms approval-inbox --repo %s validate" % repo)

# --- task packet: contradictions the status command tolerates ---
task = load_json("task.json")
if task.get("present"):
    status = task.get("status") or ""
    closed_at = task.get("closed_at") or ""
    last = task.get("last_activity") or ""
    if status == "active" and closed_at:
        finding("fail", "task", "packet is active but carries closed_at=%s" % closed_at,
                "oms agent-task --repo %s close --reason '<why>'" % repo)
    if last and not re.match(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$", last):
        finding("warn", "task",
                "last_activity %r does not parse, so staleness checks silently pass" % last,
                "oms agent-task --repo %s update" % repo)
    if status == "active" and task.get("stale"):
        finding("warn", "task", "active packet is stale (no activity within the TTL)",
                "oms agent-task --repo %s close --reason '<why>' # or update it" % repo)

# --- CURRENT pointers: a pointer that names a missing target ---
current = os.path.join(oms, "threads", "CURRENT")
if os.path.isfile(current):
    try:
        head = open(current).readline().split()
    except OSError:
        head = []
    thread_id = head[0] if head else ""
    if thread_id and not os.path.isfile(os.path.join(oms, "threads", thread_id + ".jsonl")):
        finding("fail", "threads", "CURRENT names %s but no such thread file exists" % thread_id,
                "rm %s" % current)

current = os.path.join(oms, "runs", "CURRENT")
spine = os.path.join(oms, "runs", "spine.jsonl")
if os.path.isfile(current) and os.path.isfile(spine):
    try:
        head = open(current).readline().split()
        run_id = head[0] if head else ""
        known = run_id and run_id in open(spine).read()
    except OSError:
        run_id, known = "", True
    if run_id and not known:
        finding("fail", "runs", "CURRENT names %s but the spine has no such run" % run_id,
                "rm %s" % current)

# --- delegations: markers whose worker process is gone ---
orphans = 0
for marker in sorted(glob.glob(os.path.join(oms, "delegations", "*.json"))):
    try:
        pid = int(json.load(open(marker)).get("pid") or 0)
    except Exception:
        continue
    if pid <= 0:
        continue
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        orphans += 1
    except PermissionError:
        pass
if orphans:
    finding("warn", "delegations", "%d delegation marker(s) reference dead processes" % orphans,
            "oms gc --repo %s --apply" % repo)

# --- contract violations inside .oms itself ---
if not os.path.isfile(os.path.join(oms, ".gitignore")):
    finding("fail", "oms", ".oms/.gitignore is missing; harness state could reach git",
            "printf '*\\n' > %s/.gitignore" % oms)
locks = [p for p in glob.glob(os.path.join(oms, "**", "*.lock"), recursive=True)]
if locks:
    finding("fail", "oms", "%d lock entr(ies) live inside .oms; locks belong in OMS_LOCK_DIR"
            % len(locks), "oms cleanup --apply  # after checking %s" % locks[0])

# --- work journal: derived views behind their source events ---
journal = load_json("journal.json")
if journal.get("enabled"):
    dirty = int(journal.get("dirty_period_count") or 0)
    if dirty:
        finding("warn", "work-journal", "%d derived period(s) are behind events.jsonl" % dirty,
                "oms journal rebuild --repo %s" % repo)
    if journal.get("event_count") and not journal.get("index_ready"):
        finding("warn", "work-journal", "event index is not ready",
                "oms journal rebuild --repo %s" % repo)

fails = sum(1 for f in findings if f["level"] == "fail")
warns = sum(1 for f in findings if f["level"] == "warn")

if as_json:
    print(json.dumps({
        "schema": 1,
        "repo": repo,
        "adopted": True,
        "findings": findings,
        "summary": {"fail": fails, "warn": warns},
        "delegated": {
            "run_validate_exit": run_exit,
            "artifact_index_exit": artifact_exit,
            "lifecycle_exit": lifecycle_exit,
            "approval_exit": approval_exit,
        },
    }, indent=2, sort_keys=True))
else:
    for f in findings:
        print("%s: %s: %s  (remedy: %s)" % (f["level"], f["family"], f["message"], f["remedy"]))
    if findings:
        print("state-verify: %d finding(s) (%d fail, %d warn)" % (len(findings), fails, warns))
    else:
        print("state-verify: ok")

sys.exit(1 if findings else 0)
PY
