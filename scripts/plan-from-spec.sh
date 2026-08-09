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

usage() {
  cat <<'EOF'
Usage: plan-from-spec.sh [--repo PATH] [--to PROVIDER] [--max-tasks N]
       plan-from-spec.sh [--repo PATH] --apply PROPOSAL.json

Read the repository's PROJECT.md contract (Goal/Scope/Non-goals, Commands,
Verification) and ask the selected peer to decompose the remaining work into
plan tasks. The result is written as a PROPOSAL under .oms/plan/ and printed
for review — nothing touches the task board until --apply.

  --repo PATH     Repository with a PROJECT.md (default: current directory).
  --to PROVIDER   Peer for the decomposition call (default: codex).
  --max-tasks N   Cap on proposed tasks (default 6, max 12).
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
    --apply) [ "$#" -ge 2 ] || fail "--apply requires a file"; APPLY_FILE="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

REPO="$(oms_repo_root "$REPO")" || fail "bad --repo"
SPEC="$REPO/PROJECT.md"
PLAN_DIR="$REPO/.oms/plan"

# Shared validator: a proposal is usable only when every task is mechanically
# safe to hand to plan-run (id shape, non-empty scope, non-empty verify,
# dependencies that stay inside the proposal).
validate_proposal() {  # FILE -> prints "ok <count>" or fails with reason
  python3 - "$1" "$MAX_TASKS" <<'PY'
import json, re, sys

try:
    data = json.load(open(sys.argv[1], encoding="utf-8"))
except (OSError, ValueError) as exc:
    sys.stderr.write("proposal is not valid JSON: %s\n" % exc)
    sys.exit(3)
tasks = data.get("tasks")
if not isinstance(tasks, list) or not tasks:
    sys.stderr.write("proposal carries no tasks\n")
    sys.exit(3)
if len(tasks) > int(sys.argv[2]):
    sys.stderr.write("proposal exceeds the task cap (%d)\n" % int(sys.argv[2]))
    sys.exit(3)
id_re = re.compile(r"^[A-Za-z0-9._-]+$")
seen = []
for t in tasks:
    if not isinstance(t, dict):
        sys.stderr.write("task entries must be objects\n"); sys.exit(3)
    tid = t.get("id") or ""
    if not id_re.match(tid):
        sys.stderr.write("bad task id: %r\n" % tid); sys.exit(3)
    if tid in seen:
        sys.stderr.write("duplicate task id: %s\n" % tid); sys.exit(3)
    if not (t.get("title") or "").strip():
        sys.stderr.write("task %s has no title\n" % tid); sys.exit(3)
    allowed = t.get("allowed")
    if not isinstance(allowed, list) or not [a for a in allowed if str(a).strip()]:
        sys.stderr.write("task %s has empty allowed paths\n" % tid); sys.exit(3)
    if any(str(a).startswith("/") or ".." in str(a) for a in allowed):
        sys.stderr.write("task %s allowed paths must be repo-relative\n" % tid); sys.exit(3)
    if not (t.get("verify") or "").strip():
        sys.stderr.write("task %s has no verify command\n" % tid); sys.exit(3)
    for dep in t.get("depends") or []:
        if dep not in seen:
            sys.stderr.write("task %s depends on unknown/later task %s\n" % (tid, dep)); sys.exit(3)
    seen.append(tid)
print("ok %d" % len(tasks))
PY
}

spec_field() {  # BULLET-PREFIX -> first bullet value
  sed -n "s/^- $1:[[:space:]]*//p" "$SPEC" | sed -n 1p
}

spec_section() {  # HEADER -> section body
  awk -v h="## $1" '$0 == h {f=1; next} /^## / {f=0} f' "$SPEC"
}

if [ -n "$APPLY_FILE" ]; then
  [ -f "$APPLY_FILE" ] || fail "no proposal at $APPLY_FILE"
  out="$(validate_proposal "$APPLY_FILE")" || exit 3
  count="${out#ok }"
  if [ ! -f "$PLAN_DIR/tasks.json" ]; then
    [ -f "$SPEC" ] || fail "no plan and no PROJECT.md to derive one from"
    goal="$(spec_field Goal)"
    accept="$(spec_section Verification | sed -n 's/^- Required checks:[[:space:]]*//p' | sed -n 1p)"
    [ -n "$accept" ] || accept="$(spec_section Commands | sed -n 's/^- Test:[[:space:]]*//p' | sed -n 1p)"
    init_args=(init --goal "${goal:-see PROJECT.md}")
    [ -z "$accept" ] || init_args+=(--accept "$accept")
    "$ROOT/scripts/agent-plan.sh" --repo "$REPO" "${init_args[@]}" >/dev/null
    echo "plan-from-spec: plan initialized from PROJECT.md${accept:+ (accept: $accept)}"
  fi
  python3 - "$APPLY_FILE" <<'PY' |
import json, sys
for t in json.load(open(sys.argv[1], encoding="utf-8"))["tasks"]:
    print("\x1f".join([
        t["id"], t["title"],
        ",".join(str(a) for a in t.get("allowed") or []),
        t["verify"],
        ",".join(str(d) for d in t.get("depends") or []),
    ]))
PY
  while IFS=$'\x1f' read -r tid title allowed verify depends; do
    add_args=(add --id "$tid" --title "$title" --allowed "$allowed" --verify "$verify")
    [ -z "$depends" ] || add_args+=(--depends "$depends")
    "$ROOT/scripts/agent-plan.sh" --repo "$REPO" "${add_args[@]}" >/dev/null
    echo "plan-from-spec: added $tid — $title"
  done
  echo "plan-from-spec: $count task(s) applied; drive with: oms goal-drive --repo $REPO"
  exit 0
fi

[ -f "$SPEC" ] || fail "no PROJECT.md in $REPO; scaffold one: apply-project-template.sh auto $REPO"
state="$(sed -n 's/^- State:[[:space:]]*//p' "$SPEC" | sed -n 1p)"
[ -n "$state" ] || fail "PROJECT.md has no '- State:' field"
if [ "$state" = "draft" ]; then
  fail "PROJECT.md is still draft; confirm the spec first (decomposing a draft automates guessing)"
fi

prompt="Decompose the remaining work for this repository into at most $MAX_TASKS
plan tasks. Ground every task in the PROJECT.md contract below; stay inside
Scope, never touch Non-goals.

Return ONLY one JSON object, no prose, matching exactly:
{\"tasks\": [{\"id\": \"t1\", \"title\": \"feat: ...\", \"allowed\": [\"path/prefix\"],
\"verify\": \"one mechanical shell command\", \"depends\": [\"earlier-id\"]}]}

Rules: ids match [A-Za-z0-9._-]+ and are unique; titles are conventional-commit
shaped (they become commit subjects); allowed is a non-empty list of
repo-relative path prefixes the task may touch; verify is one runnable command
that fails while the task is unfinished; depends names only earlier tasks in
the list; prefer few, landable, independently verifiable tasks.

--- PROJECT.md contract ---
State: $state
Goal: $(spec_field Goal)
Scope: $(spec_field Scope)
Non-goals: $(spec_field Non-goals)
Commands:
$(spec_section Commands)
Verification:
$(spec_section Verification)
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
rm -f "$raw"
[ -n "$answer_artifact" ] && [ -f "$answer_artifact" ] ||
  fail "decomposition call produced no artifact"

ts="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$PLAN_DIR"
agent_memory_ensure_oms_ignore_for_path "$PLAN_DIR" 2>/dev/null || true
proposal="$PLAN_DIR/proposal-$ts.json"
if ! python3 - "$answer_artifact" > "$proposal" <<'PY'
import json, sys

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
json.dump(best, sys.stdout, ensure_ascii=False, indent=2)
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
