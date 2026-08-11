#!/usr/bin/env bash
set -euo pipefail

# PROJECT.md -> proposed plan tasks. Decomposition is a model judgment, so by
# design this PROPOSES only: the task list becomes agent-plan state exclusively
# through an explicit --apply of a reviewed proposal file. That keeps the
# cross-family review verdict intact — generated plans enter the board through
# approval, never silently. The chain it completes:
#   PROJECT.md -> plan-from-spec (propose) -> [review] -> --apply -> goal-drive.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"

REPO="$PWD"
PROVIDER="codex"
APPLY_FILE=""
MAX_TASKS=6
ID_PREFIX=""
ALLOWED_ENVELOPE=""

usage() {
  cat <<'EOF'
Usage: plan-from-spec.sh [--repo PATH] [--to PROVIDER] [--max-tasks N]
                         [--id-prefix PREFIX] [--allowed PATHS]
       plan-from-spec.sh [--repo PATH] --apply PROPOSAL.json

Read the repository's PROJECT.md contract (Goal/Scope/Non-goals, Commands,
Verification) and ask the selected peer to decompose the remaining work into
plan tasks. The result is written as a PROPOSAL under .oms/plan/ and printed
for review — nothing touches the task board until --apply.

  --repo PATH     Repository with a PROJECT.md (default: current directory).
  --to PROVIDER   Peer for the decomposition call (default: codex).
  --max-tasks N   Cap on proposed tasks (default 6, max 12).
  --id-prefix P   Require every generated task id to start with P. Bounded
                  autopilot replans use r1- so a second tranche is observable.
  --allowed PATHS Comma-separated immutable path envelope for every proposed
                  task. Stored in the proposal and rechecked during apply.
  --apply FILE    Append a reviewed proposal's tasks to the plan. Creates the
                  plan (goal + acceptance from PROJECT.md) when absent; an
                  existing plan and its acceptance command are never replaced.

A PROJECT.md still in `- State: draft` is refused: decomposing an unconfirmed
spec automates guessing.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --to) [ "$#" -ge 2 ] || fail "--to requires a provider"; PROVIDER="$2"; shift 2 ;;
    --max-tasks)
      [ "$#" -ge 2 ] || fail "--max-tasks requires a count"
      case "$2" in *[!0-9]*|"") fail "--max-tasks requires a positive integer" ;; esac
      [ "$2" -ge 1 ] && [ "$2" -le 12 ] || fail "--max-tasks must be 1..12"
      MAX_TASKS="$2"; shift 2 ;;
    --id-prefix)
      [ "$#" -ge 2 ] || fail "--id-prefix requires a value"
      ID_PREFIX="$2"; shift 2 ;;
    --allowed)
      [ "$#" -ge 2 ] || fail "--allowed requires paths"
      ALLOWED_ENVELOPE="$2"; shift 2 ;;
    --apply) [ "$#" -ge 2 ] || fail "--apply requires a file"; APPLY_FILE="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

REPO="$(oms_repo_root "$REPO")" || fail "bad --repo"
SPEC="$REPO/PROJECT.md"
PLAN_DIR="$REPO/.oms/plan"
PLAN_FILE="$PLAN_DIR/tasks.json"

[ "${OMS_HARNESS_CHILD:-0}" != 1 ] ||
  fail "plan-from-spec is parent-only; a harness child cannot propose or apply plan topology"

case "$ID_PREFIX" in
  "") ;;
  *[!A-Za-z0-9._-]*) fail "--id-prefix must match [A-Za-z0-9._-]+" ;;
esac

# Shared validator: a proposal is usable only when every task is mechanically
# safe to hand to plan-run (id shape, non-empty scope, non-empty verify,
# dependencies that stay inside the proposal).
validate_proposal() {  # FILE [APPLY] -> prints "ok <count>" or fails with reason
  python3 - "$1" "$MAX_TASKS" "$ID_PREFIX" "$ALLOWED_ENVELOPE" "$PLAN_FILE" "${2:-0}" <<'PY'
import json, re, sys, unicodedata

try:
    data = json.load(open(sys.argv[1], encoding="utf-8"))
except (OSError, ValueError) as exc:
    sys.stderr.write("proposal is not valid JSON: %s\n" % exc)
    sys.exit(3)
top_keys = {
    "schema", "kind", "spec_sha256", "plan_sha256", "base_sha",
    "id_prefix", "allowed_envelope", "tasks",
}
if (not isinstance(data, dict) or set(data) != top_keys or data.get("schema") != 1 or
        data.get("kind") != "agent-plan-proposal"):
    sys.stderr.write("proposal does not match the exact reviewed schema\n")
    sys.exit(3)
tasks = data.get("tasks")
if not isinstance(tasks, list) or not tasks:
    sys.stderr.write("proposal carries no tasks\n")
    sys.exit(3)
if len(tasks) > int(sys.argv[2]):
    sys.stderr.write("proposal exceeds the task cap (%d)\n" % int(sys.argv[2]))
    sys.exit(3)
id_re = re.compile(r"^[A-Za-z0-9._-]+$")
prefix = sys.argv[3]
envelope_text = sys.argv[4]
plan_path = sys.argv[5]
apply_mode = sys.argv[6] == "1"

def reject_controls(value, label):
    if any(unicodedata.category(ch) in ("Cc", "Cf", "Cs") for ch in value):
        raise ValueError("%s contains a control or format character" % label)

def clean_rel(value, label):
    if not isinstance(value, str):
        raise ValueError("%s must be a string" % label)
    reject_controls(value, label)
    value = value.strip().replace("\\", "/")
    while value.startswith("./"):
        value = value[2:]
    value = value.rstrip("/") or "."
    if (value.startswith("/") or re.match(r"^[A-Za-z]:", value) or
            (value != "." and any(part in ("", ".", "..") for part in value.split("/")))):
        raise ValueError("%s must be a normalized repo-relative path" % label)
    return value

try:
    envelope = [clean_rel(item, "allowed envelope")
                for item in re.split(r"[,\s]+", envelope_text) if item.strip()]
except ValueError as exc:
    sys.stderr.write(str(exc) + "\n"); sys.exit(3)

existing = {}
try:
    with open(plan_path, encoding="utf-8") as handle:
        value = json.load(handle)
    if isinstance(value, dict) and isinstance(value.get("tasks"), dict):
        existing = value["tasks"]
except OSError:
    pass
except ValueError:
    sys.stderr.write("existing plan is invalid JSON\n"); sys.exit(3)

seen = []
for t in tasks:
    if not isinstance(t, dict):
        sys.stderr.write("task entries must be objects\n"); sys.exit(3)
    if set(t) != {"id", "title", "allowed", "verify", "depends"}:
        sys.stderr.write("task entries must match id/title/allowed/verify/depends exactly\n"); sys.exit(3)
    tid = t.get("id") or ""
    if not id_re.fullmatch(tid):
        sys.stderr.write("bad task id: %r\n" % tid); sys.exit(3)
    if prefix and not tid.startswith(prefix):
        sys.stderr.write("task id %s does not start with required prefix %s\n" % (tid, prefix)); sys.exit(3)
    if tid in seen:
        sys.stderr.write("duplicate task id: %s\n" % tid); sys.exit(3)
    # A replay of the exact proposal after an interrupted parent run is
    # resolved atomically by agent-plan, which compares every immutable task
    # field. Fresh generation still rejects duplicate ids here.
    if tid in existing and not apply_mode:
        sys.stderr.write("task id already exists in the plan: %s\n" % tid); sys.exit(3)
    if not isinstance(t.get("title"), str) or not t["title"].strip():
        sys.stderr.write("task %s has no title\n" % tid); sys.exit(3)
    try:
        reject_controls(t["title"], "task %s title" % tid)
    except ValueError as exc:
        sys.stderr.write(str(exc) + "\n"); sys.exit(3)
    allowed = t.get("allowed")
    if (not isinstance(allowed, list) or not allowed or
            any(not isinstance(a, str) or not a.strip() for a in allowed)):
        sys.stderr.write("task %s has empty allowed paths\n" % tid); sys.exit(3)
    if any(a.startswith("/") or ".." in a for a in allowed):
        sys.stderr.write("task %s allowed paths must be repo-relative\n" % tid); sys.exit(3)
    if envelope:
        try:
            cleaned = [clean_rel(a, "task allowed path") for a in allowed]
        except ValueError as exc:
            sys.stderr.write(str(exc) + "\n"); sys.exit(3)
        for candidate in cleaned:
            if not any(root == "." or candidate == root or candidate.startswith(root + "/")
                       for root in envelope):
                sys.stderr.write("task %s widens the allowed path envelope\n" % tid); sys.exit(3)
    if not isinstance(t.get("verify"), str) or not t["verify"].strip():
        sys.stderr.write("task %s has no verify command\n" % tid); sys.exit(3)
    try:
        reject_controls(t["verify"], "task %s verify" % tid)
    except ValueError as exc:
        sys.stderr.write(str(exc) + "\n"); sys.exit(3)
    depends = t.get("depends")
    if (not isinstance(depends, list) or any(not isinstance(dep, str) for dep in depends)
            or any(not id_re.fullmatch(dep) for dep in depends)
            or len(depends) != len(set(depends))):
        sys.stderr.write("task %s dependencies must be unique ids\n" % tid); sys.exit(3)
    for dep in depends:
        if dep in seen:
            continue
        if dep not in existing:
            sys.stderr.write("task %s depends on unknown/later task %s\n" % (tid, dep)); sys.exit(3)
        if existing[dep].get("state") != "done":
            sys.stderr.write("task %s depends on unfinished existing task %s\n" % (tid, dep)); sys.exit(3)
    seen.append(tid)
print("ok %d" % len(tasks))
PY
}

spec_field() {  # BULLET-PREFIX -> first bullet value
  sed -n "s/^- $1:[[:space:]]*//p" "$SPEC" | sed -n 1p | tr -d '\r'
}

spec_section() {  # HEADER -> section body
  awk -v h="## $1" '{ sub(/\r$/, "", $0) } $0 == h {f=1; next} /^## / {f=0} f' "$SPEC"
}

if [ -n "$APPLY_FILE" ]; then
  [ -f "$APPLY_FILE" ] || fail "no proposal at $APPLY_FILE"
  proposal_meta="$(python3 - "$APPLY_FILE" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    data = json.load(handle)
allowed = data.get("allowed_envelope") or []
print(data.get("spec_sha256") or "-")
print(data.get("plan_sha256") or "-")
print(",".join(str(item) for item in allowed))
print(data.get("id_prefix") or "")
PY
)"
  proposal_meta="${proposal_meta//$'\r'/}"
  proposal_spec_sha="$(printf '%s\n' "$proposal_meta" | sed -n 1p)"
  proposal_plan_sha="$(printf '%s\n' "$proposal_meta" | sed -n 2p)"
  proposal_allowed="$(printf '%s\n' "$proposal_meta" | sed -n 3p)"
  proposal_prefix="$(printf '%s\n' "$proposal_meta" | sed -n 4p)"
  if [ -n "$ID_PREFIX" ] && [ "$ID_PREFIX" != "$proposal_prefix" ]; then
    fail "--id-prefix does not match the reviewed proposal"
  fi
  ID_PREFIX="$proposal_prefix"
  case "$ID_PREFIX" in
    "") ;;
    *[!A-Za-z0-9._-]*) fail "proposal id_prefix is invalid" ;;
  esac
  out="$(validate_proposal "$APPLY_FILE" 1)" || exit 3
  out="${out//$'\r'/}"
  count="${out#ok }"
  [ -f "$SPEC" ] || fail "PROJECT.md is required to bind a proposal apply"
  goal="$(spec_field Goal)"
  accept="$(spec_section Verification | sed -n 's/^- Required checks:[[:space:]]*//p' | sed -n 1p)"
  [ -n "$accept" ] || accept="$(spec_section Commands | sed -n 's/^- Test:[[:space:]]*//p' | sed -n 1p)"
  [ -n "$accept" ] || fail "PROJECT.md has no executable verification command"

  current_spec_sha="$(oms_sha256_file "$SPEC")" || fail "cannot hash PROJECT.md"
  if [ -f "$PLAN_FILE" ]; then
    current_plan_sha="$(oms_sha256_file "$PLAN_FILE")" || fail "cannot hash current plan"
  else
    current_plan_sha=absent
  fi
  [ "$proposal_spec_sha" = - ] || [ "$proposal_spec_sha" = "$current_spec_sha" ] ||
    fail "PROJECT.md changed after the proposal was generated"
  if [ "$proposal_plan_sha" != - ] && [ "$proposal_plan_sha" != "$current_plan_sha" ]; then
    # Exact replay after atomic apply is resolved by agent-plan from immutable
    # task definitions; every other stale plan is rejected there.
    expected_plan_sha="$proposal_plan_sha"
  else
    expected_plan_sha="$current_plan_sha"
  fi
  # agent-plan performs the authoritative clean/sort/set comparison. Keep the
  # wrapper representation-agnostic so `src/` and `src`, or a different list
  # order, remain the same reviewed boundary.
  apply_allowed="${ALLOWED_ENVELOPE:-$proposal_allowed}"
  [ -n "$apply_allowed" ] || apply_allowed=.
  proposal_sha="$(oms_sha256_file "$APPLY_FILE")" || fail "cannot hash proposal"

  "$ROOT/scripts/agent-plan.sh" --repo "$REPO" apply-proposal \
    --proposal "$APPLY_FILE" \
    --expected-proposal-sha256 "$proposal_sha" \
    --expected-plan-sha256 "$expected_plan_sha" \
    --goal "${goal:-see PROJECT.md}" --accept "$accept" \
    --allowed-envelope "$apply_allowed" --max-tasks "$MAX_TASKS" >/dev/null
  echo "plan-from-spec: $count task(s) applied; drive with: oms goal-drive --repo $REPO"
  exit 0
fi

[ -f "$SPEC" ] || fail "no PROJECT.md in $REPO; scaffold one: apply-project-template.sh auto $REPO"
state="$(sed -n 's/^- State:[[:space:]]*//p' "$SPEC" | sed -n 1p)"
state="${state//$'\r'/}"
case "$state" in
  confirmed|active) ;;
  draft) fail "PROJECT.md is still draft; confirm the spec first (decomposing a draft automates guessing)" ;;
  "") fail "PROJECT.md has no '- State:' field" ;;
  *) fail "PROJECT.md State must be confirmed (legacy active is also accepted)" ;;
esac

spec_sha="$(oms_sha256_file "$SPEC")" || fail "cannot hash PROJECT.md"
if [ -f "$PLAN_FILE" ]; then
  plan_sha="$(oms_sha256_file "$PLAN_FILE")" || fail "cannot hash current plan"
  plan_context="$(python3 - "$PLAN_FILE" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    data = json.load(handle)
for task in data.get("tasks", {}).values():
    print("- %s | %s | %s | allowed=%s" % (
        task.get("id", "?"), task.get("state", "?"), task.get("title", ""),
        ",".join(task.get("allowed_paths") or [])))
PY
)"
  plan_context="${plan_context//$'\r'/}"
else
  plan_sha=absent
  plan_context="- (no plan yet)"
fi
base_sha="$(git -C "$REPO" rev-parse HEAD 2>/dev/null || true)"
[ -n "$base_sha" ] || fail "repository has no HEAD commit"
prefix_rule="ids match [A-Za-z0-9._-]+ and are unique"
[ -z "$ID_PREFIX" ] || prefix_rule="every id starts with $ID_PREFIX and otherwise matches [A-Za-z0-9._-]+"
scope_rule="stay inside PROJECT.md Scope"
[ -z "$ALLOWED_ENVELOPE" ] || scope_rule="every allowed path stays inside this immutable envelope: $ALLOWED_ENVELOPE"

prompt="Decompose the remaining work for this repository into at most $MAX_TASKS
plan tasks. Ground every task in the PROJECT.md contract below; stay inside
Scope, never touch Non-goals.

Return ONLY one JSON object, no prose, matching exactly:
{\"tasks\": [{\"id\": \"t1\", \"title\": \"feat: ...\", \"allowed\": [\"path/prefix\"],
\"verify\": \"one mechanical shell command\", \"depends\": [\"earlier-id\"]}]}

Rules: $prefix_rule; titles are conventional-commit
shaped (they become commit subjects); allowed is a non-empty list of
repo-relative path prefixes the task may touch; verify is one runnable command
that fails while the task is unfinished; depends names only earlier tasks in
the list or an existing DONE task; $scope_rule; never repeat or modify an
existing task; prefer few, landable, independently verifiable tasks.

--- PROJECT.md contract ---
State: $state
Goal: $(spec_field Goal)
Scope: $(spec_field Scope)
Non-goals: $(spec_field Non-goals)
Commands:
$(spec_section Commands)
Verification:
$(spec_section Verification)
Existing plan (id | state | title | scope):
$plan_context
--- end contract ---"

raw="$(agent_memory_mktemp)" || fail "mktemp failed"
if ! "$ROOT/scripts/agent-call.sh" --to "$PROVIDER" --repo "$REPO" \
    --operation plan --prompt "$prompt" > "$raw" 2>&1; then
  echo "error: decomposition call failed:" >&2
  tail -n 5 "$raw" >&2
  rm -f "$raw"
  exit 3
fi
# agent-call prints only the artifact path; the answer body lives inside it.
answer_artifact="$(sed -n 's/^artifact: //p' "$raw" | tail -n 1)"
answer_artifact="${answer_artifact//$'\r'/}"
rm -f "$raw"
[ -n "$answer_artifact" ] && [ -f "$answer_artifact" ] ||
  fail "decomposition call produced no artifact"

ts="$(date -u +%Y%m%dT%H%M%SZ)-$$"
mkdir -p "$PLAN_DIR"
agent_memory_ensure_oms_ignore_for_path "$PLAN_DIR" 2>/dev/null || true
proposal="$PLAN_DIR/proposal-$ts.json"
if ! OMS_PLAN_SPEC_SHA="$spec_sha" OMS_PLAN_BEFORE_SHA="$plan_sha" \
  OMS_PLAN_BASE_SHA="$base_sha" OMS_PLAN_ID_PREFIX="$ID_PREFIX" \
  OMS_PLAN_ALLOWED="$ALLOWED_ENVELOPE" \
  python3 - "$answer_artifact" > "$proposal" <<'PY'
import json, os, re, sys

text = open(sys.argv[1], encoding="utf-8", errors="replace").read()
# Some provider CLIs echo the whole prompt (schema example included) inside
# their transcript, so scan every balanced JSON object and keep the LAST one
# carrying a "tasks" key — the answer always follows the echo.
marker = text.find("## Output")
if marker >= 0:
    text = text[marker:]
decoder = json.JSONDecoder()
best = None
idx = 0
while True:
    start = text.find("{", idx)
    if start < 0:
        break
    try:
        obj, end = decoder.raw_decode(text, start)
    except ValueError:
        idx = start + 1
        continue
    if isinstance(obj, dict) and "tasks" in obj:
        best = obj
    idx = end if end > start else start + 1
if best is None:
    sys.stderr.write("no tasks JSON object in the provider answer\n")
    sys.exit(3)
allowed = [item for item in re.split(r"[,\s]+", os.environ.get("OMS_PLAN_ALLOWED", "")) if item]
wrapped = {
    "schema": 1,
    "kind": "agent-plan-proposal",
    "spec_sha256": os.environ["OMS_PLAN_SPEC_SHA"],
    "plan_sha256": os.environ["OMS_PLAN_BEFORE_SHA"],
    "base_sha": os.environ["OMS_PLAN_BASE_SHA"],
    "id_prefix": os.environ.get("OMS_PLAN_ID_PREFIX", ""),
    "allowed_envelope": allowed,
    "tasks": best.get("tasks"),
}
json.dump(wrapped, sys.stdout, ensure_ascii=False, indent=2)
PY
then
  rm -f "$proposal"
  echo "error: could not extract a proposal; raw answer kept at $answer_artifact" >&2
  exit 3
fi

out="$(validate_proposal "$proposal")" || {
  echo "error: proposal failed validation; inspect $proposal" >&2
  exit 3
}
out="${out//$'\r'/}"
count="${out#ok }"
echo "plan-from-spec: proposed $count task(s) -> $proposal"
python3 - "$proposal" <<'PY'
import json, sys
for t in json.load(open(sys.argv[1], encoding="utf-8"))["tasks"]:
    deps = ",".join(t.get("depends") or []) or "-"
    print("  %-14s %s  [allowed: %s] [verify: %s] [depends: %s]" % (
        t["id"], t["title"], ",".join(t["allowed"]), t["verify"], deps))
PY
echo "review the list, then: oms plan-from-spec --repo $REPO --apply $proposal"
exit 0
