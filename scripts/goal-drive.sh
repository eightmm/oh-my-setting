#!/usr/bin/env bash
set -euo pipefail

# Bounded goal driver: loop [acceptance -> one plan-run task -> exact commit]
# over an EXISTING human-approved plan until the plan's acceptance command
# passes, the cycle cap is hit, tasks run out, or anything looks unsafe. This
# is the mechanized form of the parent's re-orientation between tasks — the
# acceptance check, stuck check, and park reasons ARE the re-orientation —
# not an unbounded autonomy loop. Deliberately absent in v1, in line with the
# cross-family design review: no plan generation, no replanning, no automatic
# lease reclaim, no default repair. The driver only ever commits the exact
# files the admission gate applied; any surprise in the tree parks the run.
#
# Preconditions: clean tree (it refuses otherwise — run it in a dedicated
# worktree when other sessions share the checkout), a plan with tasks and an
# acceptance command (agent-plan init --goal ... --accept CMD). The
# acceptance command is snapshotted at start; if it changes mid-run the run
# parks rather than judge against a moved goalpost.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"

REPO="$PWD"
PROVIDER="codex"
MAX_CYCLES=3
AUTO_REPAIR=0

usage() {
  cat <<'EOF'
Usage: goal-drive.sh [--repo PATH] [--to PROVIDER] [--max-cycles N] [--auto-repair]

Drive an existing agent-plan toward its acceptance command: each cycle runs
acceptance first (pass = done), otherwise executes exactly one plan task via
plan-run --next --land and commits the landed patch (and nothing else).

Stops by design: acceptance pass; --max-cycles (the hard terminator, default
3); no actionable task while acceptance still fails; identical tree + same
failing acceptance twice (stuck); acceptance command changed mid-run; any
mismatch between the admitted patch and the working tree. Every terminal
writes a reason row to .oms/plan/progress.jsonl and a fail-ledger entry when
it parked.

  --repo PATH     Repository (default: current directory). Tree must be clean.
  --to PROVIDER   Worker for plan-run delegation (default: codex).
  --max-cycles N  Hard cycle cap (default 3, max 10).
  --auto-repair   Pass through to plan-run --land: one repair round on a
                  failed landing before parking.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --to) [ "$#" -ge 2 ] || fail "--to requires a provider"; PROVIDER="$2"; shift 2 ;;
    --max-cycles)
      [ "$#" -ge 2 ] || fail "--max-cycles requires a count"
      case "$2" in *[!0-9]*|"") fail "--max-cycles requires a positive integer" ;; esac
      [ "$2" -ge 1 ] && [ "$2" -le 10 ] || fail "--max-cycles must be 1..10"
      MAX_CYCLES="$2"; shift 2 ;;
    --auto-repair) AUTO_REPAIR=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

REPO="$(oms_repo_root "$REPO")" || fail "bad --repo"
PLAN_FILE="$REPO/.oms/plan/tasks.json"
PROGRESS="$REPO/.oms/plan/progress.jsonl"
[ -f "$PLAN_FILE" ] || fail "no plan at $PLAN_FILE; create one first (agent-plan init/add)"

read_accept_cmd() {
  python3 -c '
import json, sys
try:
    d = json.load(open(sys.argv[1], encoding="utf-8"))
except Exception:
    sys.exit(0)
print(d.get("accept", "") or "")
' "$PLAN_FILE"
}

ACCEPT_CMD="$(read_accept_cmd)"
[ -n "$ACCEPT_CMD" ] ||
  fail "plan has no acceptance command; set one: agent-plan init --goal ... --accept CMD"
ACCEPT_SNAPSHOT="$(printf '%s' "$ACCEPT_CMD" | oms_sha256_stream 2>/dev/null || echo unhashed)"

# A dirty tree means either another session's live work or an interrupted
# landing; committing around it is exactly the incident this driver refuses
# to repeat. No override flag on purpose.
dirty="$(git -C "$REPO" status --porcelain --untracked-files=no)"
[ -z "$dirty" ] || fail "tree is dirty; commit or stash first, or drive from a dedicated worktree"

RUN_ID="gd-$(date -u +%Y%m%dT%H%M%SZ)-$$"
CYCLE=0

terminal_row() {  # STATUS REASON
  OMS_GD_TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)" OMS_GD_RUN="$RUN_ID" \
    OMS_GD_CYCLE="$CYCLE" OMS_GD_STATUS="$1" OMS_GD_REASON="$2" \
    OMS_GD_SHA="$(git -C "$REPO" rev-parse HEAD 2>/dev/null || echo unborn)" \
    python3 -c '
import json, os
print(json.dumps({
    "schema": 1, "kind": "terminal", "ts": os.environ["OMS_GD_TS"],
    "run_id": os.environ["OMS_GD_RUN"], "cycle": int(os.environ["OMS_GD_CYCLE"]),
    "base_sha": os.environ["OMS_GD_SHA"], "status": os.environ["OMS_GD_STATUS"],
    "reason": os.environ["OMS_GD_REASON"],
}, ensure_ascii=False))
' >> "$PROGRESS" 2>/dev/null || true
}

park() {  # REASON NEXT
  terminal_row park "$1"
  "$ROOT/scripts/fail-ledger.sh" record --repo "$REPO" --kind plan-run \
    --cmd "goal-drive $RUN_ID" --exit 3 --summary "parked: $1" \
    --next "$2" >/dev/null 2>&1 || true
  echo "goal-drive: parked run=$RUN_ID cycle=$CYCLE reason=$1"
  echo "goal-drive: next: $2"
  exit 3
}

prev_tree=""
prev_accept_out=""

while :; do
  CYCLE=$((CYCLE + 1))
  # Judge against the start-of-run contract only: a mid-run edit to the
  # acceptance command is a moved goalpost, not a pass. Checked in the main
  # shell so park() actually terminates the run.
  now="$(printf '%s' "$(read_accept_cmd)" | oms_sha256_stream 2>/dev/null || echo unhashed)"
  [ "$now" = "$ACCEPT_SNAPSHOT" ] ||
    park "acceptance-command-changed" "re-run goal-drive after reviewing the new acceptance command"
  accept_rc=0
  accept_out="$(OMS_GOAL_RUN_ID="$RUN_ID" OMS_GOAL_CYCLE="$CYCLE" \
    "$ROOT/scripts/agent-plan.sh" --repo "$REPO" accept 2>&1)" || accept_rc=$?
  printf '%s\n' "$accept_out" | tail -n 3
  if [ "$accept_rc" -eq 0 ]; then
    terminal_row "done" acceptance-pass
    echo "goal-drive: done run=$RUN_ID cycles=$CYCLE (acceptance passed)"
    exit 0
  fi
  [ "$accept_rc" -eq 3 ] ||
    park "acceptance-command-error" "the acceptance command itself failed to run; fix the spec (agent-plan init --accept)"

  # Stuck: nothing changed since the last failing cycle — more cycles would
  # replay the same failure, not fix it.
  tree="$(git -C "$REPO" rev-parse 'HEAD^{tree}' 2>/dev/null || echo none)"
  accept_digest="$(printf '%s' "$accept_out" | oms_sha256_stream 2>/dev/null || echo unhashed)"
  if [ "$tree" = "$prev_tree" ] && [ "$accept_digest" = "$prev_accept_out" ]; then
    park "stuck" "same tree and same failing acceptance twice; get an outside read: oms advise"
  fi
  prev_tree="$tree"; prev_accept_out="$accept_digest"

  if [ "$CYCLE" -gt "$MAX_CYCLES" ]; then
    park "cycles-exhausted" "acceptance still failing after $MAX_CYCLES cycle(s); review progress.jsonl, then extend the plan or raise --max-cycles"
  fi

  head_before="$(git -C "$REPO" rev-parse HEAD)"
  run_args=(--repo "$REPO" --to "$PROVIDER" --next --land)
  [ "$AUTO_REPAIR" -eq 0 ] || run_args+=(--auto-repair)
  run_rc=0
  run_out="$("$ROOT/scripts/plan-run.sh" "${run_args[@]}" 2>&1)" || run_rc=$?
  printf '%s\n' "$run_out" | tail -n 6
  if [ "$run_rc" -ne 0 ]; then
    if printf '%s\n' "$run_out" | grep -q 'no actionable task'; then
      park "tasks-exhausted" "acceptance fails and the plan has no actionable task; add tasks (agent-plan add) or decompose the remaining gap"
    fi
    park "task-failed" "plan-run failed (see its ledger row); review the task, then re-run goal-drive"
  fi

  task_id="$(printf '%s\n' "$run_out" | sed -n 's/^plan-run: result task=\([^ ]*\).*/\1/p' | tail -n 1)"
  patch_file="$(printf '%s\n' "$run_out" | sed -n 's/.* patch=\([^ ]*\).*/\1/p' | tail -n 1)"

  # Commit exactly what admission applied. The admitted patch is the
  # authority on which paths changed (patch-land applies without --index, so
  # a brand-new file lands untracked and would be invisible to a
  # tracked-only status check). Stage those paths by name, never add -A.
  [ "$(git -C "$REPO" rev-parse HEAD)" = "$head_before" ] ||
    park "head-moved" "another writer advanced HEAD mid-cycle; inspect git log, then re-run on a quiet tree"
  if [ -z "$patch_file" ] || [ ! -f "$patch_file" ]; then
    park "no-admitted-patch" "cannot verify the landing against its patch; land manually (oms patch-land)"
  fi
  paths_tmp="$(mktemp)" || park "internal" "mktemp failed; land manually"
  git -C "$REPO" apply --numstat "$patch_file" 2>/dev/null | cut -f3 |
    sed '/^$/d' > "$paths_tmp"
  [ -s "$paths_tmp" ] ||
    park "no-admitted-patch" "the admitted patch names no paths; land manually"
  # Tracked modifications beyond the patch mean someone else's work is in the
  # tree; committing would launder it into this task's commit.
  extra="$(git -C "$REPO" status --porcelain --untracked-files=no | sed 's/^...//' |
    grep -Fxv -f "$paths_tmp" | sed '/^$/d' || true)"
  if [ -n "$extra" ]; then
    rm -f "$paths_tmp"
    park "commit-mismatch" "tracked changes outside the admitted patch: $(printf '%s' "$extra" | tr '\n' ' '); resolve by hand"
  fi

  title="$("$ROOT/scripts/agent-plan.sh" --repo "$REPO" show --id "$task_id" 2>/dev/null |
    python3 -c 'import json,sys
try: print(json.load(sys.stdin).get("title",""))
except Exception: print("")')"
  msg="$(printf '%s' "${title:-$task_id}" | tr -d '\000-\037' | sed 's/^[-[:space:]]*//' | cut -c1-72)"
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    # A deleted tracked path stages as a deletion; a path that neither exists
    # nor is tracked would make git add fail the whole run, so skip it.
    if [ -e "$REPO/$f" ] || git -C "$REPO" ls-files --error-unmatch -- "$f" >/dev/null 2>&1; then
      git -C "$REPO" add -- "$f"
    fi
  done < "$paths_tmp"
  rm -f "$paths_tmp"
  if git -C "$REPO" diff --cached --quiet; then
    park "empty-landing" "plan-run reported success but the admitted paths show no change; inspect the task artifact"
  fi
  git -C "$REPO" commit -q -m "${msg:-plan task $task_id}"
  after_dirty="$(git -C "$REPO" status --porcelain --untracked-files=no)"
  [ -z "$after_dirty" ] ||
    park "commit-incomplete" "tracked changes remain after the task commit; resolve by hand"
  echo "goal-drive: committed task=$task_id $(git -C "$REPO" rev-parse --short HEAD)"
done
