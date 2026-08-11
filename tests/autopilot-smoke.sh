#!/usr/bin/env bash
set -euo pipefail

# The bounded autopilot is orchestration, not a second landing engine. These
# regressions pin the new contracts at its two mutation boundaries: a proposal
# is appended to the plan atomically, and only a fully completed tranche may
# trigger one scope-fenced remainder proposal.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-autopilot.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

sha256_file() {
  python3 - "$1" <<'PY'
import hashlib, sys
with open(sys.argv[1], "rb") as handle:
    print(hashlib.sha256(handle.read()).hexdigest())
PY
}

make_repo() {
  local repo="$1"
  mkdir -p "$repo"
  git -C "$repo" init -q
  git -C "$repo" config user.email test@example.com
  git -C "$repo" config user.name Test
  printf '/.oms/\n/calls/\n/*.out\n' > "$repo/.gitignore"
  mkdir -p "$repo/src" "$repo/tests"
  printf 'base\n' > "$repo/src/app.txt"
  cat > "$repo/PROJECT.md" <<'EOF'
# PROJECT.md

## Status

- State: active

## Project

- Goal: finish the bounded feature
- Scope: src/, tests/
- Non-goals: release

## Commands

- Test: true

## Verification

- Required checks: true
EOF
  git -C "$repo" add .gitignore PROJECT.md src/app.txt
  git -C "$repo" commit -qm base
  git -C "$repo" branch -M main
  git -C "$repo" switch -qc codex/fixture
}

write_proposal() {
  local path="$1"
  local second_allowed="${2:-tests/}"
  local repo spec_sha base_sha
  repo="$(dirname "$path")"
  spec_sha="$(sha256_file "$repo/PROJECT.md")"
  base_sha="$(git -C "$repo" rev-parse HEAD)"
  cat > "$path" <<EOF
{
  "schema": 1,
  "kind": "agent-plan-proposal",
  "spec_sha256": "$spec_sha",
  "plan_sha256": "absent",
  "base_sha": "$base_sha",
  "id_prefix": "",
  "allowed_envelope": ["tests/", "src/"],
  "tasks": [
    {"id":"t1","title":"feat: core","allowed":["src/"],"verify":"true","depends":[]},
    {"id":"t2","title":"test: core","allowed":["$second_allowed"],"verify":"true","depends":["t1"]}
  ]
}
EOF
}

test_atomic_proposal_apply() {
  local repo="$TMP/atomic"
  local proposal before after proposal_sha

  make_repo "$repo"
  proposal="$repo/proposal.json"
  write_proposal "$proposal"
  proposal_sha="$(sha256_file "$proposal")"

  "$ROOT/scripts/agent-plan.sh" --repo "$repo" apply-proposal \
    --proposal "$proposal" \
    --expected-proposal-sha256 "$proposal_sha" \
    --expected-plan-sha256 absent \
    --goal "finish the bounded feature" --accept true \
    --allowed-envelope 'src/,tests/' --max-tasks 4 >/dev/null ||
    fail "a reviewed proposal should apply atomically"

  python3 - "$repo/.oms/plan/tasks.json" <<'PY' || fail "proposal tasks missing"
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    data = json.load(handle)
assert data["goal"] == "finish the bounded feature"
assert data["accept"] == "true"
assert data["project_contract"]["schema"] == 1
assert data["project_contract"]["spec_sha256"]
assert data["project_contract"]["allowed_envelope"] == ["src", "tests"]
assert list(data["tasks"]) == ["t1", "t2"]
assert data["tasks"]["t2"]["depends"] == ["t1"]
PY

  # Crash recovery may replay the exact apply. It is idempotent when every
  # immutable task field still matches, even if task runtime state later moved.
  "$ROOT/scripts/agent-plan.sh" --repo "$repo" apply-proposal \
    --proposal "$proposal" \
    --expected-proposal-sha256 "$proposal_sha" \
    --expected-plan-sha256 absent \
    --goal "finish the bounded feature" --accept true \
    --allowed-envelope 'src/,tests/' --max-tasks 4 > "$repo/replay.out" ||
    fail "exact proposal replay should converge"
  grep -Fq 'already applied' "$repo/replay.out" ||
    fail "proposal replay did not report convergence"

  # A documented resume may pass the same reviewed proposal after goal-drive
  # has legitimately committed work. With the durable contract already in
  # place, exact replay is read-only and does not require the old HEAD.
  printf 'legitimate implementation\n' >> "$repo/src/app.txt"
  git -C "$repo" add src/app.txt
  git -C "$repo" commit -qm 'feat: advance reviewed work'
  before="$(sha256_file "$repo/.oms/plan/tasks.json")"
  "$ROOT/scripts/plan-from-spec.sh" --repo "$repo" --apply "$proposal" \
    --allowed 'src,tests' --max-tasks 4 >/dev/null ||
    fail "exact proposal replay after an implementation commit should converge"
  [ "$before" = "$(sha256_file "$repo/.oms/plan/tasks.json")" ] ||
    fail "HEAD-relaxed exact replay changed the approved plan"

  if "$ROOT/scripts/agent-plan.sh" --repo "$repo" add --id bypass \
      --title 'fix: bypass review' --allowed src --verify true >/dev/null 2>&1; then
    fail "manual add bypassed a contract-bound reviewed plan"
  fi

  before="$(sha256_file "$repo/.oms/plan/tasks.json")"
  write_proposal "$proposal" 'outside/'
  proposal_sha="$(sha256_file "$proposal")"
  if "$ROOT/scripts/agent-plan.sh" --repo "$repo" apply-proposal \
    --proposal "$proposal" \
    --expected-proposal-sha256 "$proposal_sha" \
    --expected-plan-sha256 "$before" \
    --allowed-envelope 'src/,tests/' --max-tasks 4 >/dev/null 2>&1; then
    fail "scope-widening proposal should be rejected"
  fi
  after="$(sha256_file "$repo/.oms/plan/tasks.json")"
  [ "$before" = "$after" ] || fail "failed proposal partially changed the plan"

  cat > "$proposal" <<'EOF'
{"schema":1,"kind":"agent-plan-proposal","tasks":[{"id":"t3","title":"fix: follow-up","allowed":["src/"],"verify":"true","depends":[]}]}
EOF
  proposal_sha="$(sha256_file "$proposal")"
  if "$ROOT/scripts/agent-plan.sh" --repo "$repo" apply-proposal \
    --proposal "$proposal" \
    --expected-proposal-sha256 "$proposal_sha" \
    --expected-plan-sha256 0000000000000000000000000000000000000000000000000000000000000000 \
    --allowed-envelope 'src/,tests/' --max-tasks 4 >/dev/null 2>&1; then
    fail "stale plan CAS should be rejected"
  fi
  [ "$before" = "$(sha256_file "$repo/.oms/plan/tasks.json")" ] ||
    fail "stale proposal CAS changed the plan"

  # The proposal's source revision and confirmed spec are part of the atomic
  # apply contract, not advisory metadata.
  local stale_repo="$TMP/stale-proposal"
  local stale_proposal stale_sha
  make_repo "$stale_repo"
  stale_proposal="$stale_repo/proposal.json"
  write_proposal "$stale_proposal"
  printf '\nchanged after review\n' >> "$stale_repo/PROJECT.md"
  stale_sha="$(sha256_file "$stale_proposal")"
  if "$ROOT/scripts/agent-plan.sh" --repo "$stale_repo" apply-proposal \
    --proposal "$stale_proposal" --expected-proposal-sha256 "$stale_sha" \
    --expected-plan-sha256 absent --goal g --accept true \
    --allowed-envelope 'src,tests' --max-tasks 4 >/dev/null 2>&1; then
    fail "a proposal bound to an older PROJECT.md should not apply"
  fi
  [ ! -f "$stale_repo/.oms/plan/tasks.json" ] ||
    fail "stale spec proposal changed the plan"

  local head_repo="$TMP/stale-head"
  local head_proposal head_sha
  make_repo "$head_repo"
  head_proposal="$head_repo/proposal.json"
  write_proposal "$head_proposal"
  printf 'clean intervening commit\n' >> "$head_repo/src/app.txt"
  git -C "$head_repo" add src/app.txt
  git -C "$head_repo" commit -qm 'chore: move proposal base'
  head_sha="$(sha256_file "$head_proposal")"
  if "$ROOT/scripts/agent-plan.sh" --repo "$head_repo" apply-proposal \
    --proposal "$head_proposal" --expected-proposal-sha256 "$head_sha" \
    --expected-plan-sha256 absent --goal g --accept true \
    --allowed-envelope 'src,tests' --max-tasks 4 >/dev/null 2>&1; then
    fail "a proposal bound to an older clean HEAD should not apply"
  fi
  [ ! -f "$head_repo/.oms/plan/tasks.json" ] || fail "stale HEAD proposal changed the plan"

  local draft_repo="$TMP/draft-proposal"
  local draft_proposal draft_sha
  make_repo "$draft_repo"
  sed -i.bak 's/- State: active/- State: draft/' "$draft_repo/PROJECT.md"
  rm -f "$draft_repo/PROJECT.md.bak"
  git -C "$draft_repo" add PROJECT.md
  git -C "$draft_repo" commit -qm 'docs: return project contract to draft'
  draft_proposal="$draft_repo/proposal.json"
  write_proposal "$draft_proposal"
  draft_sha="$(sha256_file "$draft_proposal")"
  if "$ROOT/scripts/agent-plan.sh" --repo "$draft_repo" apply-proposal \
      --proposal "$draft_proposal" --expected-proposal-sha256 "$draft_sha" \
      --expected-plan-sha256 absent --goal g --accept true \
      --allowed-envelope 'src,tests' >/dev/null 2>&1; then
    fail "a current but unconfirmed PROJECT.md should not authorize plan topology"
  fi
  [ ! -f "$draft_repo/.oms/plan/tasks.json" ] || fail "draft proposal changed plan state"

  local control_repo="$TMP/control-proposal"
  local control_proposal control_sha
  make_repo "$control_repo"
  control_proposal="$control_repo/proposal.json"
  write_proposal "$control_proposal"
  python3 - "$control_proposal" <<'PY'
import json, sys
path = sys.argv[1]
with open(path, encoding="utf-8") as handle:
    row = json.load(handle)
row["tasks"][0]["verify"] = "true\t."
with open(path, "w", encoding="utf-8") as handle:
    json.dump(row, handle)
PY
  control_sha="$(sha256_file "$control_proposal")"
  if "$ROOT/scripts/agent-plan.sh" --repo "$control_repo" apply-proposal \
    --proposal "$control_proposal" --expected-proposal-sha256 "$control_sha" \
    --expected-plan-sha256 absent --goal g --accept true \
    --allowed-envelope 'src,tests' --max-tasks 4 >/dev/null 2>&1; then
    fail "task delimiter injection should be rejected"
  fi
  [ ! -f "$control_repo/.oms/plan/tasks.json" ] ||
    fail "control-character proposal partially changed the plan"

  write_proposal "$control_proposal"
  python3 - "$control_proposal" <<'PY'
import json, sys
path = sys.argv[1]
with open(path, encoding="utf-8") as handle:
    row = json.load(handle)
row["tasks"][0]["role"] = "hidden-strategy"
with open(path, "w", encoding="utf-8") as handle:
    json.dump(row, handle)
PY
  if "$ROOT/scripts/plan-from-spec.sh" --repo "$control_repo" \
      --apply "$control_proposal" --allowed 'src,tests' >/dev/null 2>&1; then
    fail "proposal accepted a hidden task field absent from the review schema"
  fi

  write_proposal "$control_proposal"
  python3 - "$control_proposal" <<'PY'
import json, sys
path = sys.argv[1]
with open(path, encoding="utf-8") as handle:
    row = json.load(handle)
row["tasks"][0]["title"] = "\u001b[2Jspoofed review"
with open(path, "w", encoding="utf-8") as handle:
    json.dump(row, handle)
PY
  control_sha="$(sha256_file "$control_proposal")"
  if "$ROOT/scripts/agent-plan.sh" --repo "$control_repo" apply-proposal \
      --proposal "$control_proposal" --expected-proposal-sha256 "$control_sha" \
      --expected-plan-sha256 absent --goal g --accept true \
      --allowed-envelope 'src,tests' >/dev/null 2>&1; then
    fail "proposal accepted a terminal-control review spoof"
  fi

  # Plan topology belongs to the parent. Low-level entry points must enforce
  # the same child boundary as the autopilot wrapper, and manual plans must not
  # accept delimiters that shift plan-run's tab-serialized fields.
  local child_repo="$TMP/child-topology"
  local child_proposal child_sha
  make_repo "$child_repo"
  child_proposal="$child_repo/proposal.json"
  write_proposal "$child_proposal"
  child_sha="$(sha256_file "$child_proposal")"
  if OMS_HARNESS_CHILD=1 "$ROOT/scripts/agent-plan.sh" --repo "$child_repo" apply-proposal \
      --proposal "$child_proposal" --expected-proposal-sha256 "$child_sha" \
      --expected-plan-sha256 absent --goal g --accept true \
      --allowed-envelope 'src,tests' >/dev/null 2>&1; then
    fail "a harness child applied plan topology directly"
  fi
  if OMS_HARNESS_CHILD=1 "$ROOT/scripts/plan-from-spec.sh" --repo "$child_repo" \
      --apply "$child_proposal" --allowed 'src,tests' >/dev/null 2>&1; then
    fail "a harness child applied plan topology through plan-from-spec"
  fi
  if OMS_HARNESS_CHILD=1 "$ROOT/scripts/agent-plan.sh" --repo "$child_repo" \
      init --goal g --accept true >/dev/null 2>&1; then
    fail "a harness child initialized parent plan state"
  fi
  [ ! -f "$child_repo/.oms/plan/tasks.json" ] || fail "child topology denial wrote plan state"
  "$ROOT/scripts/agent-plan.sh" --repo "$child_repo" init --goal g --accept true >/dev/null
  if OMS_HARNESS_CHILD=1 "$ROOT/scripts/agent-plan.sh" --repo "$child_repo" add \
      --id child --title 'fix: child task' --allowed src --verify true >/dev/null 2>&1; then
    fail "a harness child added a task directly"
  fi
  if "$ROOT/scripts/agent-plan.sh" --repo "$child_repo" add --id shifted \
      --title 'fix: shifted task' --allowed src --verify $'true\t.' >/dev/null 2>&1; then
    fail "manual add accepted a control delimiter"
  fi
  if "$ROOT/scripts/agent-plan.sh" --repo "$child_repo" add --id $'shifted\n' \
      --title 'fix: shifted id' --allowed src --verify true >/dev/null 2>&1; then
    fail "manual add accepted a newline-suffixed id"
  fi
  python3 - "$child_repo/.oms/plan/tasks.json" <<'PY' || fail "rejected manual tasks changed plan topology"
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    assert json.load(handle)["tasks"] == {}
PY

  # A legacy plan can be adopted only when the reviewed proposal names every
  # task. Extra tasks must not silently gain autopilot authority.
  local legacy_repo="$TMP/legacy-adoption"
  local legacy_proposal legacy_plan_sha legacy_proposal_sha
  make_repo "$legacy_repo"
  "$ROOT/scripts/agent-plan.sh" --repo "$legacy_repo" init --goal g --accept true >/dev/null
  "$ROOT/scripts/agent-plan.sh" --repo "$legacy_repo" add --id t1 --title 'feat: core' \
    --allowed src --verify true >/dev/null
  "$ROOT/scripts/agent-plan.sh" --repo "$legacy_repo" add --id t2 --title 'test: core' \
    --depends t1 --allowed tests --verify true >/dev/null
  "$ROOT/scripts/agent-plan.sh" --repo "$legacy_repo" add --id extra --title 'fix: unreviewed' \
    --allowed src --verify true >/dev/null
  legacy_plan_sha="$(sha256_file "$legacy_repo/.oms/plan/tasks.json")"
  legacy_proposal="$legacy_repo/proposal.json"
  write_proposal "$legacy_proposal" tests
  python3 - "$legacy_proposal" "$legacy_plan_sha" <<'PY'
import json, sys
path = sys.argv[1]
with open(path, encoding="utf-8") as handle:
    row = json.load(handle)
row["plan_sha256"] = sys.argv[2]
with open(path, "w", encoding="utf-8") as handle:
    json.dump(row, handle)
PY
  legacy_proposal_sha="$(sha256_file "$legacy_proposal")"
  if "$ROOT/scripts/agent-plan.sh" --repo "$legacy_repo" apply-proposal \
      --proposal "$legacy_proposal" --expected-proposal-sha256 "$legacy_proposal_sha" \
      --expected-plan-sha256 "$legacy_plan_sha" --goal g --accept true \
      --allowed-envelope 'src,tests' >/dev/null 2>&1; then
    fail "legacy adoption admitted a task absent from the reviewed proposal"
  fi

  # Section parsing is portable across a full Windows CRLF project contract.
  local crlf_apply_repo="$TMP/crlf-apply"
  local crlf_apply_proposal
  make_repo "$crlf_apply_repo"
  python3 - "$crlf_apply_repo/PROJECT.md" <<'PY'
import sys
path = sys.argv[1]
raw = open(path, "rb").read().replace(b"\r\n", b"\n").replace(b"\n", b"\r\n")
open(path, "wb").write(raw)
PY
  git -C "$crlf_apply_repo" add PROJECT.md
  git -C "$crlf_apply_repo" commit -qm 'docs: use CRLF project contract'
  crlf_apply_proposal="$crlf_apply_repo/proposal.json"
  write_proposal "$crlf_apply_proposal"
  "$ROOT/scripts/plan-from-spec.sh" --repo "$crlf_apply_repo" \
    --apply "$crlf_apply_proposal" --allowed 'src,tests' --max-tasks 4 >/dev/null ||
    fail "full-CRLF PROJECT.md should retain its verification contract"
}

write_done_plan() {
  local repo="$1"
  local accept="$2"
  local include_replan="${3:-0}"
  mkdir -p "$repo/.oms/plan"
  OMS_T_ACCEPT="$accept" OMS_T_REPLAN="$include_replan" python3 - "$repo/.oms/plan/tasks.json" <<'PY'
import datetime, json, os, sys
now = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")
tasks = {
    "t1": {
        "id": "t1", "title": "feat: initial", "state": "done",
        "depends": [], "allowed_paths": ["src/"], "forbidden_paths": [],
        "verify": "true", "role": "", "provider": "codex", "ttl": "",
        "artifact": "artifact", "patch": "patch", "reason": "",
        "executor_id": "", "executor_soul_sha256": "", "lease_epoch": 1,
        "lease_id": "lease", "review_lease_id": "lease", "repair_count": 0,
        "repair_artifact": "", "created": now, "updated": now,
    }
}
if os.environ["OMS_T_REPLAN"] == "1":
    tasks["r1-finish"] = dict(tasks["t1"], id="r1-finish", title="fix: remainder")
with open(sys.argv[1], "w", encoding="utf-8") as handle:
    spec = open(os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(sys.argv[1]))), "PROJECT.md"), "rb").read()
    import hashlib
    json.dump({
        "schema": 3,
        "goal": "finish the bounded feature",
        "accept": os.environ["OMS_T_ACCEPT"],
        "project_contract": {
            "schema": 1,
            "spec_sha256": hashlib.sha256(spec).hexdigest(),
            "allowed_envelope": ["src", "tests"],
        },
        "tasks": tasks,
    }, handle)
PY
}

write_orchestration_stubs() {
  local bin="$1"
  mkdir -p "$bin"

  cat > "$bin/goal-drive" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$OMS_T_CALLS/goal-drive"
if [ "${OMS_T_GOAL_COMMIT:-0}" = 1 ]; then
  printf 'implemented by goal-drive\n' >> "$OMS_T_REPO/src/app.txt"
  git -C "$OMS_T_REPO" add src/app.txt
  git -C "$OMS_T_REPO" commit -qm 'feat: implement planned work'
fi
case "${OMS_T_GOAL_RESULT:-success}" in
  success) echo 'goal-drive: done run=test cycles=1 (acceptance passed)'; exit 0 ;;
  exhausted) echo 'goal-drive: parked run=test cycle=1 reason=tasks-exhausted'; exit 3 ;;
  *) echo 'goal-drive: parked run=test cycle=1 reason=task-failed'; exit 3 ;;
esac
EOF

  cat > "$bin/plan-from-spec" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$OMS_T_CALLS/plan-from-spec"
[ "${OMS_T_PLAN_RC:-0}" = 0 ] || exit "$OMS_T_PLAN_RC"
proposal="$OMS_T_REPO/.oms/plan/proposal-r1.json"
mkdir -p "$(dirname "$proposal")"
cat > "$proposal" <<'JSON'
{"schema":1,"kind":"agent-plan-proposal","tasks":[{"id":"r1-finish","title":"fix: finish remainder","allowed":["src/"],"verify":"true","depends":["t1"]}]}
JSON
echo "plan-from-spec: proposed 1 task(s) -> $proposal"
echo "review the list, then: oms plan-from-spec --repo $OMS_T_REPO --apply $proposal"
EOF

cat > "$bin/peer-review" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$OMS_T_CALLS/peer-review"
[ "${OMS_T_REVIEW_BREAK_ACCEPT:-0}" != 1 ] || : > "$OMS_T_CALLS/fail-accept"
if [ "${OMS_T_REVIEW_COMMIT:-0}" = 1 ]; then
  printf 'reviewer mutation\n' >> "$OMS_T_REPO/src/app.txt"
  git -C "$OMS_T_REPO" add src/app.txt
  git -C "$OMS_T_REPO" commit -qm 'chore: reviewer mutation'
fi
exit "${OMS_T_REVIEW_RC:-0}"
EOF

  cat > "$bin/draft-pr" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$OMS_T_CALLS/draft-pr"
case "${1:-}" in
  prepare)
    intent="$OMS_T_REPO/.oms/publish/fake.json"
    mkdir -p "$(dirname "$intent")"
    printf '{}\n' > "$intent"
    echo "intent: $intent"
    ;;
  publish) echo 'draft-pr: published https://example.invalid/pr/1' ;;
esac
EOF
  chmod +x "$bin"/*
}

run_autopilot() {
  local repo="$1"
  shift
  OMS_T_REPO="$repo" OMS_T_CALLS="$repo/calls" \
    OMS_AUTOPILOT_GOAL_DRIVE="$TMP/bin/goal-drive" \
    OMS_AUTOPILOT_PLAN_FROM_SPEC="$TMP/bin/plan-from-spec" \
    OMS_AUTOPILOT_PEER_REVIEW="$TMP/bin/peer-review" \
    OMS_AUTOPILOT_DRAFT_PR="$TMP/bin/draft-pr" \
    "$ROOT/scripts/autopilot.sh" --repo "$repo" "$@"
}

test_autopilot_orchestration() {
  local repo="$TMP/orchestrate"
  local contract_repo="$TMP/contract-resume"
  local branch_repo="$TMP/branch-start"
  local base_review_repo="$TMP/frozen-base-review"
  local review_repo="$TMP/review-mutation"
  local rc=0

  make_repo "$repo"
  mkdir -p "$repo/calls"
  write_orchestration_stubs "$TMP/bin"
  write_done_plan "$repo" false

  OMS_T_GOAL_RESULT=exhausted run_autopilot "$repo" run \
    --planner claude --worker codex --allowed 'src/,tests/' --base main \
    > "$repo/replan.out" 2>&1 || rc=$?
  [ "$rc" = 4 ] || fail "task exhaustion should propose one bounded replan, got $rc"
  grep -Fq 'proposal-r1.json' "$repo/replan.out" || fail "replan proposal pointer missing"
  grep -Fq -- '--id-prefix r1-' "$repo/calls/plan-from-spec" ||
    fail "replan did not fence generated ids"
  grep -Fq -- '--max-tasks 2' "$repo/calls/plan-from-spec" ||
    fail "replan task cap missing"
  grep -Fq -- '--allowed src,tests' "$repo/calls/plan-from-spec" ||
    fail "replan scope envelope missing"

  # An existing r1 tranche proves the bounded replan was already consumed.
  : > "$repo/calls/plan-from-spec"
  write_done_plan "$repo" false 1
  rc=0
  OMS_T_GOAL_RESULT=exhausted run_autopilot "$repo" run \
    --planner claude --worker codex --allowed 'src/,tests/' --base main \
    > "$repo/second-replan.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "a second replan should park, got $rc"
  [ ! -s "$repo/calls/plan-from-spec" ] || fail "planner was called after replan budget"

  # A resume cannot widen the original plan envelope, and a committed spec
  # edit invalidates the durable plan contract before any worker call.
  make_repo "$contract_repo"
  mkdir -p "$contract_repo/calls"
  write_done_plan "$contract_repo" false
  rc=0
  OMS_T_GOAL_RESULT=exhausted run_autopilot "$contract_repo" run \
    --planner claude --worker codex --allowed . --base main \
    > "$contract_repo/scope.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "resume scope widening should be a contract error, got $rc"
  [ ! -f "$contract_repo/calls/plan-from-spec" ] ||
    fail "scope widening reached the planner"

  printf '\nchanged contract\n' >> "$contract_repo/PROJECT.md"
  git -C "$contract_repo" add PROJECT.md
  git -C "$contract_repo" commit -qm 'docs: change project contract'
  rc=0
  OMS_T_GOAL_RESULT=success run_autopilot "$contract_repo" run \
    --planner claude --worker codex --base main > "$contract_repo/spec.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "changed PROJECT.md should invalidate resume, got $rc"
  [ ! -f "$contract_repo/calls/goal-drive" ] || fail "changed spec reached goal-drive"

  # Planner failure is a parked/error result, never a fake approval boundary.
  local planner_repo="$TMP/planner-failure"
  make_repo "$planner_repo"
  mkdir -p "$planner_repo/calls"
  rc=0
  OMS_T_PLAN_RC=3 run_autopilot "$planner_repo" propose --planner claude \
    --allowed 'src,tests' > "$planner_repo/planner.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "planner failure should propagate, got $rc"
  if grep -Fq 'awaits parent review' "$planner_repo/planner.out"; then
    fail "planner failure was misreported as a proposal"
  fi

  local crlf_repo="$TMP/crlf-draft"
  make_repo "$crlf_repo"
  mkdir -p "$crlf_repo/calls"
  python3 - "$crlf_repo/PROJECT.md" <<'PY'
import sys
path = sys.argv[1]
raw = open(path, "rb").read().replace(b"- State: active\n", b"- State: draft\r\n")
open(path, "wb").write(raw)
PY
  git -C "$crlf_repo" add PROJECT.md
  git -C "$crlf_repo" commit -qm 'docs: crlf draft spec'
  rc=0
  run_autopilot "$crlf_repo" propose --allowed 'src,tests' >/dev/null 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "CRLF draft spec should be refused, got $rc"
  [ ! -f "$crlf_repo/calls/plan-from-spec" ] || fail "CRLF draft reached the planner"

  # Semantic review is shadow by default: its failure is visible but cannot
  # block a mechanically accepted draft PR. Gate mode is the opt-in strict path.
  write_done_plan "$repo" true
  : > "$repo/calls/peer-review"
  : > "$repo/calls/draft-pr"
  OMS_T_GOAL_RESULT=success OMS_T_REVIEW_RC=1 run_autopilot "$repo" run \
    --planner claude --worker codex --reviewer claude --allowed 'src/,tests/' \
    --base main --draft-pr > "$repo/shadow.out" 2>&1 ||
    fail "shadow semantic finding should not block a draft PR"
  grep -Fq 'semantic review: advisory fail' "$repo/shadow.out" ||
    fail "shadow review failure was hidden"
  grep -Fq 'prepare --repo' "$repo/calls/draft-pr" ||
    fail "draft intent was not prepared: $(cat "$repo/calls/draft-pr")"
  grep -Fq 'publish --repo' "$repo/calls/draft-pr" ||
    fail "draft intent was not published: $(cat "$repo/calls/draft-pr")"

  : > "$repo/calls/draft-pr"
  rc=0
  OMS_T_GOAL_RESULT=success OMS_T_REVIEW_RC=1 run_autopilot "$repo" run \
    --planner claude --worker codex --reviewer claude --allowed 'src/,tests/' \
    --base main --draft-pr --review-mode gate > "$repo/gate.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "strict semantic gate should park, got $rc"
  [ ! -s "$repo/calls/draft-pr" ] || fail "failed semantic gate reached remote publish"

  # Shadow applies only to semantic dissent. A verifier that becomes red
  # during review remains a hard stop, and reviewer-side writes invalidate the
  # exact whole-change snapshot before remote publication.
  write_done_plan "$repo" '[ ! -f calls/fail-accept ]'
  rm -f "$repo/calls/fail-accept"
  : > "$repo/calls/draft-pr"
  rc=0
  OMS_T_GOAL_RESULT=success OMS_T_REVIEW_RC=1 OMS_T_REVIEW_BREAK_ACCEPT=1 \
    run_autopilot "$repo" run --worker codex --reviewer claude --base main \
      --draft-pr > "$repo/mechanical-shadow.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "post-review mechanical failure should park, got $rc"
  [ ! -s "$repo/calls/draft-pr" ] || fail "mechanical review failure reached Draft PR"

  make_repo "$review_repo"
  mkdir -p "$review_repo/calls"
  write_done_plan "$review_repo" true
  rc=0
  OMS_T_GOAL_RESULT=success OMS_T_REVIEW_RC=0 OMS_T_REVIEW_COMMIT=1 \
    run_autopilot "$review_repo" run --worker codex --reviewer claude --base main \
      --draft-pr > "$review_repo/mutation.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "reviewer clean commit should invalidate publication, got $rc"
  [ ! -f "$review_repo/calls/draft-pr" ] || fail "unreviewed clean commit reached Draft PR"

  # A local run that starts on its base branch moves to the deterministic work
  # branch before goal-drive. The named base stays stable across interruptions,
  # and the reviewer receives its frozen object id rather than a mutable name.
  make_repo "$base_review_repo"
  mkdir -p "$base_review_repo/calls"
  write_done_plan "$base_review_repo" true
  git -C "$base_review_repo" checkout -q main
  local frozen_base
  frozen_base="$(git -C "$base_review_repo" rev-parse HEAD)"
  OMS_T_GOAL_RESULT=success OMS_T_GOAL_COMMIT=1 OMS_T_REVIEW_RC=0 \
    run_autopilot "$base_review_repo" run --worker codex --reviewer claude \
      --base main --review-mode gate > "$base_review_repo/review.out" 2>&1 ||
    fail "local base-branch run should retain a non-empty review range"
  grep -Fq -- "--base $frozen_base" "$base_review_repo/calls/peer-review" ||
    fail "semantic review did not use the frozen pre-drive base commit"
  [ "$(git -C "$base_review_repo" rev-parse main)" = "$frozen_base" ] ||
    fail "autopilot advanced the named base branch"
  [ "$(git -C "$base_review_repo" branch --show-current)" != main ] ||
    fail "local autopilot did not move work off the base branch"

  # Starting the publish path on the base branch creates a deterministic
  # local feature branch before the first implementation cycle.
  make_repo "$branch_repo"
  mkdir -p "$branch_repo/calls"
  write_done_plan "$branch_repo" true
  git -C "$branch_repo" checkout -q main
  OMS_T_GOAL_RESULT=success OMS_T_REVIEW_RC=0 run_autopilot "$branch_repo" run \
    --planner claude --worker codex --reviewer claude --base main --draft-pr \
    > "$branch_repo/branch.out" 2>&1 || fail "base-branch publish path should start safely"
  [ "$(git -C "$branch_repo" branch --show-current)" != main ] ||
    fail "autopilot left implementation commits on the base branch"

  # A second remainder proposal is rejected even when passed directly, and
  # proposal metadata cannot claim r1- while carrying ordinary ids.
  write_done_plan "$repo" true 1
  local second_proposal="$repo/.oms/plan/second-r1.json"
  cat > "$second_proposal" <<'JSON'
{"schema":1,"kind":"agent-plan-proposal","id_prefix":"r1-","allowed_envelope":["src","tests"],"tasks":[{"id":"r1-second","title":"fix: second remainder","allowed":["src"],"verify":"true","depends":[]}]}
JSON
  rc=0
  run_autopilot "$repo" run --proposal "$second_proposal" --allowed 'src,tests' \
    --worker codex --base main >/dev/null 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "direct second remainder should be rejected, got $rc"

  local bad_prefix="$contract_repo/.oms/plan/bad-prefix.json"
  local bad_spec bad_plan bad_base
  bad_spec="$(sha256_file "$contract_repo/PROJECT.md")"
  bad_plan="$(sha256_file "$contract_repo/.oms/plan/tasks.json")"
  bad_base="$(git -C "$contract_repo" rev-parse HEAD)"
  cat > "$bad_prefix" <<JSON
{"schema":1,"kind":"agent-plan-proposal","spec_sha256":"$bad_spec","plan_sha256":"$bad_plan","base_sha":"$bad_base","id_prefix":"r1-","allowed_envelope":["src","tests"],"tasks":[{"id":"ordinary","title":"fix: bad prefix","allowed":["src"],"verify":"true","depends":[]}]}
JSON
  rc=0
  "$ROOT/scripts/plan-from-spec.sh" --repo "$contract_repo" --apply "$bad_prefix" \
    --allowed 'src,tests' > "$contract_repo/bad-prefix.out" 2>&1 || rc=$?
  [ "$rc" != 0 ] || fail "proposal id_prefix metadata was not enforced"
  grep -Fq 'does not start with required prefix r1-' "$contract_repo/bad-prefix.out" ||
    fail "bad prefix was rejected for the wrong reason"

  rc=0
  OMS_HARNESS_CHILD=1 run_autopilot "$repo" run --worker codex \
    --allowed 'src/' --base main >/dev/null 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "a harness child must not drive the parent workflow"
}

test_atomic_proposal_apply
test_autopilot_orchestration

echo "autopilot-smoke: ok"
