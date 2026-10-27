#!/usr/bin/env bash
set -euo pipefail

# The bounded autopilot is orchestration, not a second landing engine. These
# regressions pin the new contracts at its two mutation boundaries: a proposal
# is appended to the plan atomically, and only a fully completed tranche may
# trigger one scope-fenced remainder proposal.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-autopilot.XXXXXX")"
trap '[ "${KEEP_TMP:-0}" = 1 ] || rm -rf "$TMP"' EXIT HUP INT TERM

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
- Required check files: PROJECT.md
EOF
  git -C "$repo" add .gitignore PROJECT.md src/app.txt
  git -C "$repo" commit -qm base
  git -C "$repo" branch -M main
  # The deterministic work branch for this fixture contract: autopilot only
  # drives its own oms/autopilot-<spec-digest> branch (or the base).
  git -C "$repo" switch -qc \
    "oms/autopilot-$(sha256_file "$repo/PROJECT.md" | cut -c1-12)"
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
  "acceptance_files": ["PROJECT.md"],
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
  python3 - "$proposal" <<'PY'
import json,sys
path=sys.argv[1]
with open(path, encoding="utf-8") as handle:
    data=json.load(handle)
data["tasks"][0]["assignment"]={"provider":"claude", "workload":"routine"}
with open(path,"w",encoding="utf-8") as handle:
    json.dump(data,handle)
PY
  proposal_sha="$(sha256_file "$proposal")"

  "$ROOT/scripts/agent-plan.sh" --repo "$repo" apply-proposal \
    --proposal "$proposal" \
    --expected-proposal-sha256 "$proposal_sha" \
    --expected-plan-sha256 absent \
    --goal "finish the bounded feature" --accept true \
    --allowed-envelope 'src/,tests/' --accept-files PROJECT.md --max-tasks 4 >/dev/null ||
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
assert data["tasks"]["t1"]["assignment"] == {"provider":"claude", "workload":"routine"}
PY

  # Crash recovery may replay the exact apply. It is idempotent when every
  # immutable task field still matches, even if task runtime state later moved.
  "$ROOT/scripts/agent-plan.sh" --repo "$repo" apply-proposal \
    --proposal "$proposal" \
    --expected-proposal-sha256 "$proposal_sha" \
    --expected-plan-sha256 absent \
    --goal "finish the bounded feature" --accept true \
    --allowed-envelope 'src/,tests/' --accept-files PROJECT.md --max-tasks 4 > "$repo/replay.out" ||
    fail "exact proposal replay should converge"
  grep -Fq 'already applied' "$repo/replay.out" ||
    fail "proposal replay did not report convergence"

  local changed_assignment="$repo/changed-assignment.json"
  python3 - "$proposal" "$changed_assignment" <<'PY'
import json,sys
with open(sys.argv[1], encoding="utf-8") as handle:
    data=json.load(handle)
data["tasks"][0]["assignment"]["provider"]="codex"
with open(sys.argv[2],"w",encoding="utf-8") as handle:
    json.dump(data,handle)
PY
  before="$(sha256_file "$repo/.oms/plan/tasks.json")"
  if "$ROOT/scripts/agent-plan.sh" --repo "$repo" apply-proposal \
      --proposal "$changed_assignment" --expected-proposal-sha256 "$(sha256_file "$changed_assignment")" \
      --expected-plan-sha256 absent --goal "finish the bounded feature" --accept true \
      --allowed-envelope 'src,tests' --accept-files PROJECT.md >"$repo/assignment-replay.out" 2>&1; then
    fail "replay changed an admitted task assignment"
  fi
  grep -Fq 'assignment does not match' "$repo/assignment-replay.out" ||
    fail "changed assignment was not rejected by immutable replay"
  [ "$before" = "$(sha256_file "$repo/.oms/plan/tasks.json")" ] || fail "assignment replay mutated plan"

  # A documented resume may pass the same reviewed proposal after goal-drive
  # has legitimately committed work. With the durable contract already in
  # place, exact replay is read-only and does not require the old HEAD.
  printf 'legitimate implementation\n' >> "$repo/src/app.txt"
  git -C "$repo" add src/app.txt
  git -C "$repo" commit -qm 'feat: advance reviewed work'
  before="$(sha256_file "$repo/.oms/plan/tasks.json")"
  "$ROOT/scripts/plan-from-spec.sh" --repo "$repo" --apply "$proposal" \
    --expected-proposal-sha256 "$proposal_sha" --allowed 'src,tests' --max-tasks 4 >/dev/null ||
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
    --allowed-envelope 'src/,tests/' --accept-files PROJECT.md --max-tasks 4 >/dev/null 2>&1; then
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
    --allowed-envelope 'src/,tests/' --accept-files PROJECT.md --max-tasks 4 >/dev/null 2>&1; then
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
    --allowed-envelope 'src,tests' --accept-files PROJECT.md --max-tasks 4 >/dev/null 2>&1; then
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
    --allowed-envelope 'src,tests' --accept-files PROJECT.md --max-tasks 4 >/dev/null 2>&1; then
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
      --allowed-envelope 'src,tests' --accept-files PROJECT.md >/dev/null 2>&1; then
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
    --allowed-envelope 'src,tests' --accept-files PROJECT.md --max-tasks 4 >/dev/null 2>&1; then
    fail "task delimiter injection should be rejected"
  fi
  [ ! -f "$control_repo/.oms/plan/tasks.json" ] ||
    fail "control-character proposal partially changed the plan"

  local assignment_case assignment_sha
  for assignment_case in null '[]' '{"workload":"routine"}' \
    '{"provider":"agy"}' '{"provider":"cursor-agent"}' \
    '{"provider":"claude","workload":"cheap"}' \
    '{"provider":"claude","model":"bad\tmodel"}' \
    '{"provider":"codex","reasoning_effort":"invalid"}' \
    '{"provider":"claude","permission":"bypass"}'; do
    write_proposal "$control_proposal"
    python3 - "$control_proposal" "$assignment_case" <<'PY'
import json,sys
path=sys.argv[1]
with open(path, encoding="utf-8") as handle:
    data=json.load(handle)
data["tasks"][0]["assignment"]=json.loads(sys.argv[2])
with open(path,"w",encoding="utf-8") as handle:
    json.dump(data,handle)
PY
    assignment_sha="$(sha256_file "$control_proposal")"
    if "$ROOT/scripts/plan-from-spec.sh" --repo "$control_repo" \
        --apply "$control_proposal" --expected-proposal-sha256 "$assignment_sha" \
        --allowed 'src,tests' >/dev/null 2>&1; then
      fail "planner accepted invalid assignment: $assignment_case"
    fi
    if "$ROOT/scripts/agent-plan.sh" --repo "$control_repo" apply-proposal \
        --proposal "$control_proposal" --expected-proposal-sha256 "$assignment_sha" \
        --expected-plan-sha256 absent --goal g --accept true \
        --allowed-envelope 'src,tests' --accept-files PROJECT.md >/dev/null 2>&1; then
      fail "atomic apply accepted invalid assignment: $assignment_case"
    fi
    [ ! -f "$control_repo/.oms/plan/tasks.json" ] || fail "invalid assignment changed plan state"
  done

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
      --apply "$control_proposal" --expected-proposal-sha256 "$(sha256_file "$control_proposal")" \
      --allowed 'src,tests' >/dev/null 2>&1; then
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
      --allowed-envelope 'src,tests' --accept-files PROJECT.md >/dev/null 2>&1; then
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
      --allowed-envelope 'src,tests' --accept-files PROJECT.md >/dev/null 2>&1; then
    fail "a harness child applied plan topology directly"
  fi
  if OMS_HARNESS_CHILD=1 "$ROOT/scripts/plan-from-spec.sh" --repo "$child_repo" \
      --apply "$child_proposal" --expected-proposal-sha256 "$child_sha" \
      --allowed 'src,tests' >/dev/null 2>&1; then
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
      --allowed-envelope 'src,tests' --accept-files PROJECT.md >/dev/null 2>&1; then
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
    --apply "$crlf_apply_proposal" \
    --expected-proposal-sha256 "$(sha256_file "$crlf_apply_proposal")" \
    --allowed 'src,tests' --max-tasks 4 >/dev/null ||
    fail "full-CRLF PROJECT.md should retain its verification contract"

  # Proposal validation and final branch admission share the same glob
  # membership semantics: a concrete task path may narrow a reviewed glob.
  local glob_plan_repo="$TMP/glob-plan-apply"
  local glob_plan_proposal
  make_repo "$glob_plan_repo"
  glob_plan_proposal="$glob_plan_repo/proposal.json"
  write_proposal "$glob_plan_proposal"
  python3 - "$glob_plan_proposal" <<'PY'
import json, sys
path = sys.argv[1]
with open(path, encoding="utf-8") as handle:
    row = json.load(handle)
row["allowed_envelope"] = ["src/*.py"]
row["tasks"] = [{
    "id": "t1", "title": "feat: glob-scoped task",
    "allowed": ["src/app.py"], "verify": "true", "depends": [],
}]
with open(path, "w", encoding="utf-8") as handle:
    json.dump(row, handle)
PY
  "$ROOT/scripts/plan-from-spec.sh" --repo "$glob_plan_repo" \
    --apply "$glob_plan_proposal" \
    --expected-proposal-sha256 "$(sha256_file "$glob_plan_proposal")" \
    --allowed 'src/*.py' --max-tasks 4 >/dev/null ||
    fail "a concrete task path should narrow its reviewed glob envelope"
}

write_done_plan() {
  local repo="$1"
  local accept="$2"
  local include_replan="${3:-0}"
  local preserve_receipt="${4:-0}"
  mkdir -p "$repo/.oms/plan"
  # Each fixture call models a fresh outer operator invocation. Individual
  # receipt-resume behavior is covered explicitly below.
  [ "$preserve_receipt" = 1 ] || rm -f "$repo/.oms/plan/autopilot-run.json"
  OMS_T_ACCEPT="$accept" OMS_T_REPLAN="$include_replan" OMS_T_WORKER="${5:-codex}" python3 - "$repo/.oms/plan/tasks.json" <<'PY'
import datetime, json, os, sys
now = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")
tasks = {
    "t1": {
        "id": "t1", "title": "feat: initial", "state": "done",
        "depends": [], "allowed_paths": ["src/"], "forbidden_paths": [],
        "verify": "true", "role": "", "provider": os.environ["OMS_T_WORKER"], "ttl": "",
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
            "acceptance_files": ["PROJECT.md"],
            "acceptance_manifest": [
                {"path": "PROJECT.md", "sha256": hashlib.sha256(spec).hexdigest()}
            ],
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
printf '%s\n' "$@" > "$OMS_T_CALLS/goal-drive-argv"
printf 'strict=%s off=%s timeout=%s\n' \
  "${OMS_WORKER_GUARD_STRICT:-}" "${OMS_WORKER_GUARD_OFF:-}" \
  "${OMS_PEER_TIMEOUT:-}" >> "$OMS_T_CALLS/goal-drive-env"
run_id=""
expected_ref=""
receipt="1111111111111111111111111111111111111111111111111111111111111111"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --run-id) run_id="$2"; shift 2 ;;
    --expected-ref) expected_ref="$2"; shift 2 ;;
    *) shift ;;
  esac
done
[ -n "$run_id" ] || exit 2
if [ -n "${OMS_T_GOAL_EXPECT_REF:-}" ]; then
  printf '%s\n' "$(git -C "$OMS_T_REPO" symbolic-ref -q HEAD)" \
    > "$OMS_T_CALLS/goal-drive-observed-ref"
  [ -z "${OMS_T_GOAL_SWITCH_REF:-}" ] ||
    git -C "$OMS_T_REPO" symbolic-ref HEAD "$OMS_T_GOAL_SWITCH_REF"
  printf '%s\n' "$expected_ref" > "$OMS_T_CALLS/goal-drive-expected-ref"
fi
if [ "${OMS_T_GOAL_COMMIT:-0}" = 1 ]; then
  printf 'implemented by goal-drive\n' >> "$OMS_T_REPO/src/app.txt"
  git -C "$OMS_T_REPO" add src/app.txt
  git -C "$OMS_T_REPO" commit -qm 'feat: implement planned work'
fi
if [ "${OMS_T_GOAL_COMMIT_OUTSIDE:-0}" = 1 ]; then
  printf 'outside reviewed scope\n' > "$OMS_T_REPO/outside.txt"
  git -C "$OMS_T_REPO" add outside.txt
  git -C "$OMS_T_REPO" commit -qm 'chore: drive outside reviewed scope'
fi
if [ "${OMS_T_GOAL_COMMIT_GLOB:-0}" = 1 ]; then
  printf 'print("inside reviewed glob")\n' > "$OMS_T_REPO/src/app.py"
  git -C "$OMS_T_REPO" add src/app.py
  git -C "$OMS_T_REPO" commit -qm 'feat: drive inside reviewed glob'
fi
if [ "${OMS_T_GOAL_ASSUME_UNCHANGED:-0}" = 1 ]; then
  git -C "$OMS_T_REPO" update-index --assume-unchanged src/app.txt
  printf 'hidden drive mutation\n' >> "$OMS_T_REPO/src/app.txt"
fi
if [ "${OMS_T_GOAL_INSTALL_FILTER:-0}" = 1 ]; then
  marker="$OMS_T_REPO/calls/filter-fired"
  git -C "$OMS_T_REPO" config filter.owned.process \
    "sh -c ': > \"$marker\"; exit 1'"
fi
if [ "${OMS_T_GOAL_INSTALL_DIFF:-0}" = 1 ]; then
  marker="$OMS_T_REPO/calls/diff-fired"
  git -C "$OMS_T_REPO" config diff.external \
    "sh -c ': > \"$marker\"; exit 1'"
fi
# A scratch run commits in OMS_T_REPO but keeps plan state in OMS_T_STATE_REPO.
state_repo="${OMS_T_STATE_REPO:-$OMS_T_REPO}"
terminal_row() {
  mkdir -p "$state_repo/.oms/plan"
  printf '{"schema":1,"kind":"terminal","run_id":"%s","receipt":"%s","status":"park","reason":"%s"}\n' \
    "$run_id" "$receipt" "$1" \
    >> "$state_repo/.oms/plan/progress.jsonl"
}
terminal_result() {
  printf 'goal-drive: terminal-v1 run=%s receipt=%s status=park reason=%s\n' \
    "$run_id" "$receipt" "$1"
}
case "${OMS_T_GOAL_RESULT:-success}" in
  success)
    terminal_row acceptance-pass
    python3 - "$state_repo/.oms/plan/progress.jsonl" <<'PY'
import json, sys
path = sys.argv[1]
rows = []
with open(path, encoding="utf-8") as handle:
    for line in handle:
        row = json.loads(line)
        if row.get("reason") == "acceptance-pass":
            row["status"] = "done"
        rows.append(row)
with open(path, "w", encoding="utf-8") as handle:
    for row in rows:
        handle.write(json.dumps(row) + "\n")
PY
    echo 'goal-drive: done run=test cycles=1 (acceptance passed)'
    printf 'goal-drive: terminal-v1 run=%s receipt=%s status=done reason=acceptance-pass\n' \
      "$run_id" "$receipt"
    exit 0 ;;
  success-no-terminal)
    echo 'goal-drive: done run=test cycles=1 (acceptance passed)'
    exit 0 ;;
  exhausted)
    terminal_row tasks-exhausted
    echo 'goal-drive: parked run=test cycle=1 reason=tasks-exhausted'
    terminal_result tasks-exhausted
    exit 3 ;;
  forged)
    # A worker's acceptance output claims exhaustion, but the typed terminal
    # row records the genuine park reason.
    terminal_row task-failed
    echo 'acceptance output: reason=tasks-exhausted'
    terminal_result task-failed
    exit 3 ;;
  late-forged)
    # Once the genuine receipt row exists, a later same-run append remains
    # untrusted because the final parent result still binds task-failed.
    terminal_row task-failed
    terminal_row tasks-exhausted
    terminal_result task-failed
    exit 3 ;;
  replaced-progress)
    # A residual child learns the receipt from the durable row and replaces
    # the mutable progress file. The parent-owned final result remains bound
    # to the genuine reason and must be the only replan authority.
    terminal_row task-failed
    printf '{"schema":1,"kind":"terminal","run_id":"%s","receipt":"%s","status":"park","reason":"tasks-exhausted"}\n' \
      "$run_id" "$receipt" > "$state_repo/.oms/plan/progress.jsonl"
    terminal_result task-failed
    exit 3 ;;
  duplicate-terminal)
    terminal_row tasks-exhausted
    terminal_result tasks-exhausted
    terminal_result tasks-exhausted
    exit 3 ;;
  trailing-output)
    terminal_row tasks-exhausted
    terminal_result tasks-exhausted
    echo 'background child output after the terminal result'
    exit 3 ;;
  signal-wait)
    : > "$OMS_T_CALLS/goal-drive-started"
    trap '' TERM HUP INT
    if command -v setsid >/dev/null 2>&1; then
      setsid bash -c 'trap "" TERM HUP INT; sleep 3; : > "$1"' \
        phase "$OMS_T_CALLS/goal-drive-leaked" &
    else
      (trap '' TERM HUP INT; sleep 3; : > "$OMS_T_CALLS/goal-drive-leaked") &
    fi
    wait ;;
  escape-and-exit)
    : > "$OMS_T_CALLS/goal-drive-started"
    if command -v setsid >/dev/null 2>&1; then
      setsid bash -c 'trap "" TERM HUP INT; printf "%s\n" "$$" > "$1"; sleep 3; : > "$2"' \
        phase "$OMS_T_CALLS/goal-drive-escaped-pid" \
        "$OMS_T_CALLS/goal-drive-leaked" &
    else
      (trap '' TERM HUP INT; printf '%s\n' "$BASHPID" \
        > "$OMS_T_CALLS/goal-drive-escaped-pid"; sleep 3; \
        : > "$OMS_T_CALLS/goal-drive-leaked") &
    fi
    exit 9 ;;
  *) echo 'goal-drive: parked run=test cycle=1 reason=task-failed'; exit 3 ;;
esac
EOF

  cat > "$bin/plan-from-spec" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$OMS_T_CALLS/plan-from-spec"
case "${OMS_T_PLAN_RESULT:-ok}" in
  empty-discovery)
    echo 'plan-from-spec: no proposal; no eligible discovery candidates'
    echo 'plan-from-spec: evidence packet: .oms/plan/work-evidence.json (sha256: fixture)'
    echo 'plan-from-spec: candidate report: .oms/plan/candidates.json (sha256: fixture)'
    exit 0 ;;
  empty)
    echo 'plan-from-spec: proposal carries no tasks' >&2
    exit 3 ;;
  unextractable)
    echo 'error: could not extract a proposal; raw answer kept at planner artifact' >&2
    exit 3 ;;
esac
if [ "${OMS_T_PLAN_RC:-0}" != 0 ]; then
  [ "$OMS_T_PLAN_RC" != 2 ] || echo 'plan-from-spec: work evidence budget exceeded' >&2
  exit "$OMS_T_PLAN_RC"
fi
proposal="$OMS_T_REPO/.oms/plan/proposal-r1.json"
mkdir -p "$(dirname "$proposal")"
cat > "$proposal" <<'JSON'
{"schema":1,"kind":"agent-plan-proposal","id_prefix":"r1-","tasks":[{"id":"r1-finish","title":"fix: finish remainder","allowed":["src/"],"verify":"true","depends":["t1"]}]}
JSON
echo "plan-from-spec: proposed 1 task(s) -> $proposal"
echo "plan-from-spec: proposal sha256: $(python3 -c \
  'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' \
  "$proposal")"
EOF

cat > "$bin/peer-review" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$OMS_T_CALLS/peer-review"
printf 'timeout=%s\n' "${OMS_PEER_TIMEOUT:-}" >> "$OMS_T_CALLS/peer-review-env"
[ "${OMS_T_REVIEW_BREAK_ACCEPT:-0}" != 1 ] || : > "$OMS_T_CALLS/fail-accept"
if [ "${OMS_T_REVIEW_COMMIT:-0}" = 1 ]; then
  printf 'reviewer mutation\n' >> "$OMS_T_REPO/src/app.txt"
  git -C "$OMS_T_REPO" add src/app.txt
  git -C "$OMS_T_REPO" commit -qm 'chore: reviewer mutation'
fi
if [ "${OMS_T_REVIEW_COMMIT_OUTSIDE:-0}" = 1 ]; then
  printf 'reviewer outside mutation\n' > "$OMS_T_REPO/outside.txt"
  git -C "$OMS_T_REPO" add outside.txt
  git -C "$OMS_T_REPO" commit -qm 'chore: reviewer outside mutation'
fi
if [ "${OMS_T_REVIEW_SKIP_WORKTREE:-0}" = 1 ]; then
  git -C "$OMS_T_REPO" update-index --skip-worktree src/app.txt
  printf 'hidden reviewer mutation\n' >> "$OMS_T_REPO/src/app.txt"
fi
if [ "${OMS_T_REVIEW_INSTALL_FSMONITOR:-0}" = 1 ]; then
  marker="$OMS_T_REPO/calls/fsmonitor-fired"
  git -C "$OMS_T_REPO" config core.fsmonitor \
    "sh -c ': > \"$marker\"; exit 1'"
fi
if [ -n "${OMS_T_REVIEW_SWITCH_BRANCH:-}" ]; then
  git -C "$OMS_T_REPO" switch -qc "$OMS_T_REVIEW_SWITCH_BRANCH"
fi
exit "${OMS_T_REVIEW_RC:-0}"
EOF

cat > "$bin/draft-pr" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$OMS_T_CALLS/draft-pr"
printf 'phase=%s wall=%s\n' "${1:-}" "${OMS_AUTOPILOT_PHASE_WALL:-}" \
  >> "$OMS_T_CALLS/draft-pr-env"
case "${1:-}" in
  prepare)
    intent="$OMS_T_REPO/.oms/publish/fake.json"
    mkdir -p "$(dirname "$intent")"
    printf '{}\n' > "$intent"
    if [ "${OMS_T_PREPARE_COMMIT_OUTSIDE:-0}" = 1 ]; then
      printf 'prepare outside mutation\n' > "$OMS_T_REPO/outside.txt"
      git -C "$OMS_T_REPO" add outside.txt
      git -C "$OMS_T_REPO" commit -qm 'chore: prepare outside mutation'
    fi
    echo "intent: $intent"
    ;;
  publish) echo 'draft-pr: published https://example.invalid/pr/1' ;;
esac
EOF

  cat > "$bin/oms" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$OMS_T_CONTINUATION_ARGS"
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

# The first checkout's refs, every checkout's HEAD, and every .oms entry by
# lstat: a symlink reads as its target and a FIFO as its type, never opened.
checkout_state() {  # CHECKOUT...
  local checkout
  git -C "$1" for-each-ref
  for checkout in "$@"; do
    git -C "$checkout" symbolic-ref --quiet HEAD || true
    git -C "$checkout" rev-parse HEAD
  done
  python3 - "$@" <<'PY'
import hashlib, os, stat, sys
rows = []
for checkout in sys.argv[1:]:
    for root, dirs, files in os.walk(os.path.join(checkout, ".oms")):
        for name in dirs + files:
            path = os.path.join(root, name)
            mode = os.lstat(path).st_mode
            if stat.S_ISLNK(mode):
                rows.append("%s -> %s" % (path, os.readlink(path)))
            elif stat.S_ISREG(mode):
                with open(path, "rb") as handle:
                    rows.append("%s %s" % (path, hashlib.sha256(handle.read()).hexdigest()))
            else:
                rows.append("%s %o" % (path, stat.S_IFMT(mode)))
print("\n".join(sorted(rows)))
PY
}

# Run a command in its own process group, killed at the suite's call timeout,
# so a call blocked on a FIFO fails with 124 instead of hanging the suite.
run_bounded() {  # OUTPUT COMMAND...
  python3 - "${OMS_TEST_CALL_TIMEOUT:-60}" "$@" <<'PY'
import os, signal, subprocess, sys
with open(sys.argv[2], "wb") as output:
    child = subprocess.Popen(sys.argv[3:], stdin=subprocess.DEVNULL, stdout=output,
                             stderr=subprocess.STDOUT, start_new_session=True)
    try:
        raise SystemExit(child.wait(timeout=float(sys.argv[1])))
    except subprocess.TimeoutExpired:
        os.killpg(child.pid, signal.SIGKILL)
        child.wait()
        raise SystemExit(124)
PY
}

test_autopilot_floor_consent() {
  local command reason rc before receipt
  local repo="$TMP/floor-consent"
  local floor_reason="--Owner reviewed baseline; preserve 'quotes' and spaces"
  for command in autopilot goal-drive plan-run; do
    rc=0
    bash "$ROOT/scripts/$command.sh" --allow-floor-change >"$TMP/floor-parse.out" 2>&1 || rc=$?
    [ "$rc" = 2 ] || fail "$command accepted a missing floor reason"
    grep -Fq 'requires a reason' "$TMP/floor-parse.out" || fail "$command missing reason diagnostic"
    for reason in '' '   ' $'one\ntwo' $'one\rtwo' $'one\ttwo' $'one\001two' $'one\177two'; do
      rc=0
      bash "$ROOT/scripts/$command.sh" --allow-floor-change "$reason" --help >"$TMP/floor-parse.out" 2>&1 || rc=$?
      [ "$rc" = 2 ] || fail "$command accepted a malformed floor reason"
      grep -Eq 'requires a (single-line|nonblank) reason|cannot contain control characters' "$TMP/floor-parse.out" ||
        fail "$command did not validate the floor reason"
    done
    for reason in 0 x --reviewed "$floor_reason"; do
      bash "$ROOT/scripts/$command.sh" --allow-floor-change "$reason" --help >"$TMP/floor-parse.out" 2>&1 ||
        fail "$command refused a valid floor reason"
      grep -Fq -- '--allow-floor-change' "$TMP/floor-parse.out" || fail "$command help omits floor consent"
    done
    rc=0
    OMS_HARNESS_CHILD=1 bash "$ROOT/scripts/$command.sh" --allow-floor-change reviewed --help >"$TMP/floor-parse.out" 2>&1 || rc=$?
    [ "$rc" = 2 ] || fail "$command allowed a child to grant floor consent"
    grep -Fq 'parent-only' "$TMP/floor-parse.out" || fail "$command child refusal omitted authority"
  done

  make_repo "$repo"
  mkdir -p "$repo/calls"
  write_done_plan "$repo" false
  write_orchestration_stubs "$TMP/bin"
  receipt="$repo/.oms/plan/autopilot-run.json"
  rc=0
  run_autopilot "$repo" propose --allowed 'src,tests' --base main \
    --allow-verifier-change --allow-floor-change "$floor_reason" >"$repo/propose.out" 2>&1 || rc=$?
  [ "$rc" = 4 ] || fail "floor consent did not bind at propose: $(tail -5 "$repo/propose.out")"
  local resume_args=(--proposal "$repo/.oms/plan/proposal-r1.json" \
    --expected-proposal-sha256 "$(sha256_file "$repo/.oms/plan/proposal-r1.json")")
  run_autopilot "$repo" status >"$repo/status.out" 2>&1 || fail "consented status failed"
  python3 - "$ROOT/scripts/lib/autopilot-receipt.py" "$receipt" "$floor_reason" \
    "$repo/propose.out" "$repo/status.out" <<'PY' || fail "floor receipt or continuation lost exact consent"
import copy, json, pathlib, runpy, shlex, sys
sys.path.insert(0, str(pathlib.Path(sys.argv[1]).parent))
api = runpy.run_path(sys.argv[1])
row = json.load(open(sys.argv[2], encoding="utf-8"))
assert row["contract"]["allow_floor_change"] == sys.argv[3]
assert row["contract"]["allow_verifier_change"] is True
for path in sys.argv[4:]:
    lines = open(path, encoding="utf-8").read().splitlines()
    argv = shlex.split(next(line for line in lines if line.startswith("  oms autopilot ")))
    assert argv[argv.index("--allow-floor-change") + 1] == sys.argv[3], argv
for invalid in (None, True, 0, "   ", "one\ntwo", "one\rtwo", "one\ttwo", "one\x7ftwo"):
    changed = copy.deepcopy(row)
    changed["contract"]["allow_floor_change"] = invalid
    try:
        api["validate_receipt"](changed)
    except api["ReceiptError"]:
        pass
    else:
        raise AssertionError(invalid)
PY
  before="$(sha256_file "$receipt")"
  for reason in '' 'changed reason'; do
    local consent_args=()
    [ -z "$reason" ] || consent_args=(--allow-floor-change "$reason")
    rc=0
    run_autopilot "$repo" run --allowed 'src,tests' --base main --allow-verifier-change \
      "${resume_args[@]}" ${consent_args[@]+"${consent_args[@]}"} >"$repo/changed.out" 2>&1 || rc=$?
    [ "$rc" = 2 ] || fail "changing floor consent must refuse, got $rc"
    grep -Fq 'immutable contract changed' "$repo/changed.out" || fail "missing immutable consent refusal"
    [ "$before" = "$(sha256_file "$receipt")" ] || fail "consent refusal changed the receipt"
    [ ! -e "$repo/calls/goal-drive" ] || fail "changed consent reached goal-drive"
  done
  rc=0
  run_autopilot "$repo" run --allowed 'src,tests' --base main --allow-verifier-change \
    "${resume_args[@]}" --allow-floor-change "$floor_reason" >"$repo/run.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "same floor reason did not resume to the drive: $(tail -5 "$repo/run.out")"
  python3 - "$repo/calls/goal-drive-argv" "$floor_reason" <<'PY' || fail "drive consent was word-split"
import sys
argv = open(sys.argv[1], encoding="utf-8").read().splitlines()
assert argv[argv.index("--allow-floor-change") + 1] == sys.argv[2], argv
assert "--allow-verifier-change" in argv
PY
  python3 - "$receipt" <<'PY' || fail "could not create legacy receipt"
import json, sys
path = sys.argv[1]
row = json.load(open(path, encoding="utf-8"))
row["contract"].pop("allow_floor_change")
with open(path, "w", encoding="utf-8") as handle:
    json.dump(row, handle)
PY
  run_autopilot "$repo" status >"$repo/legacy.out" 2>&1 || fail "legacy receipt did not load"
  if grep -Fq -- '--allow-floor-change' "$repo/legacy.out"; then fail "legacy receipt invented consent"; fi
  before="$(sha256_file "$receipt")"
  rc=0
  run_autopilot "$repo" run --allowed 'src,tests' --base main --allow-verifier-change \
    "${resume_args[@]}" --allow-floor-change "$floor_reason" >"$repo/added.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "adding floor consent must refuse"
  grep -Fq 'immutable contract changed' "$repo/added.out" || fail "missing added consent refusal"
  [ "$before" = "$(sha256_file "$receipt")" ] || fail "adding consent changed legacy receipt"
  rc=0
  run_autopilot "$repo" run --allowed 'src,tests' --base main --allow-verifier-change \
    "${resume_args[@]}" >"$repo/legacy-run.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "legacy receipt did not resume without consent: $(tail -5 "$repo/legacy-run.out")"
  if grep -Fq -- '--allow-floor-change' "$repo/calls/goal-drive-argv"; then fail "legacy drive invented consent"; fi
}

test_autopilot_abandon_plan_guidance() {
  local variant repo plan rc name task_sha
  write_orchestration_stubs "$TMP/bin"
  for variant in open no-plan empty 'done' cancelled malformed empty-file; do
    repo="$TMP/abandon plan guidance/$variant"
    make_repo "$repo"
    mkdir -p "$repo/calls"
    rc=0
    OMS_T_PLAN_RC=3 run_autopilot "$repo" propose --planner claude \
      --allowed 'src,tests' --base main > "$repo/propose.out" 2>&1 || rc=$?
    [ "$rc" = 3 ] || fail "$variant: failed to prepare an interrupted receipt ($rc)"
    plan="$repo/.oms/plan/tasks.json"
    if [ "$variant" != no-plan ]; then
      write_done_plan "$repo" true 0 1
      case "$variant" in
        malformed) printf '{' > "$plan" ;;
        empty-file) : > "$plan" ;;
        *)
          python3 - "$plan" "$variant" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    row = json.load(handle)
if sys.argv[2] == "empty":
    row["tasks"] = {}
elif sys.argv[2] in {"open", "cancelled"}:
    row["tasks"]["t1"]["state"] = "ready"
with open(sys.argv[1], "w", encoding="utf-8") as handle:
    json.dump(row, handle)
PY
          ;;
      esac
      if [ "$variant" = cancelled ]; then
        printf 'Work withdrawn by the fixture owner.\n' > "$repo/.oms/cancel-decision.txt"
        "$ROOT/scripts/agent-plan.sh" --repo "$repo" show --id t1 > "$repo/task.json"
        task_sha="$(python3 - "$repo/task.json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    print(json.load(handle)["task_sha256"])
PY
)"
        "$ROOT/scripts/agent-plan.sh" --repo "$repo" cancel --id t1 \
          --expected-plan-sha256 "$(sha256_file "$plan")" \
          --expected-task-sha256 "$task_sha" --expected-state ready \
          --expected-lease-id lease --operator-consent cancel --reason "work withdrawn" \
          --decision-artifact .oms/cancel-decision.txt \
          --expected-decision-artifact-sha256 "$(sha256_file "$repo/.oms/cancel-decision.txt")" \
          > "$repo/cancel.out" 2>&1 || fail "cancel fixture: $(cat "$repo/cancel.out")"
      fi
    fi
    for name in tasks.json progress.jsonl; do
      [ ! -e "$repo/.oms/plan/$name" ] || cp "$repo/.oms/plan/$name" "$repo/$name.before"
    done
    printf '\nOwner changed the spec after interruption.\n' >> "$repo/PROJECT.md"
    run_autopilot "$repo" abandon --reason "retire interrupted contract" \
      > "$repo/abandon.out" 2>&1 || fail "$variant: abandon failed: $(cat "$repo/abandon.out")"
    grep -Fq 'autopilot: receipt abandoned; the exact contract is preserved and the exit recorded at ' \
      "$repo/abandon.out" || fail "$variant: receipt record output changed"
    grep -Fxq 'autopilot: a fresh propose may now bind a new contract' "$repo/abandon.out" ||
      fail "$variant: fresh propose guidance changed"
    case "$variant" in
      open|malformed|empty-file)
        grep -Fxq "autopilot: plan still needs resolution: $plan" "$repo/abandon.out" ||
          fail "$variant: remaining plan path missing"
        python3 - "$repo/abandon.out" "$repo" <<'PY' || fail "$variant: retirement command quoting"
import shlex, sys
prefix = "autopilot: inspect plan retirement: "
with open(sys.argv[1], encoding="utf-8") as handle:
    command = next(line[len(prefix):] for line in handle if line.startswith(prefix))
assert shlex.split(command) == ["oms", "agent-plan", "--repo", sys.argv[2], "retire", "--check"]
PY
        grep -Fq -- '--disposition completed-external fails when acceptance files changed' "$repo/abandon.out" ||
          fail "$variant: missing completed-external constraint"
        grep -Fq -- '--disposition superseded requires every task to be done or cancelled' "$repo/abandon.out" ||
          fail "$variant: missing superseded constraint"
        grep -Fq -- '--operator-consent cancel, --reason, --decision-artifact and --expected-decision-artifact-sha256' \
          "$repo/abandon.out" || fail "$variant: missing cancellation requirements"
        ;;
      *)
        if grep -Eq 'plan still needs resolution|retire --check|--disposition|--operator-consent' "$repo/abandon.out"; then
          fail "$variant: closed or absent plan received retirement guidance"
        fi
        ;;
    esac
    for name in tasks.json progress.jsonl; do
      if [ -e "$repo/$name.before" ]; then
        cmp -s "$repo/$name.before" "$repo/.oms/plan/$name" || fail "$variant: abandon edited $name"
      else
        [ ! -e "$repo/.oms/plan/$name" ] || fail "$variant: abandon created $name"
      fi
    done
    [ ! -e "$repo/.oms/plan/retirements.jsonl" ] || fail "$variant: abandon retired the plan"
  done
}

test_autopilot_orchestration() {
  local repo="$TMP/orchestrate"
  local contract_repo="$TMP/contract-resume"
  local branch_repo="$TMP/branch-start"
  local base_review_repo="$TMP/frozen-base-review"
  local review_repo="$TMP/review-mutation"
  local terminal_case
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
  grep -Fq -- '--expected-proposal-sha256' "$repo/replan.out" ||
    fail "the printed continuation does not bind the proposal digest"
  grep -Fq 'parent-agent continuation' "$repo/replan.out" ||
    fail "the proposal continuation should be labeled as parent-agent control"
  if grep -Eiq '(user|operator).*(run|copy|paste)|you (run|copy|paste)' "$repo/replan.out"; then
    fail "autopilot should not delegate its continuation to the end user"
  fi
  grep -Fq -- '--id-prefix r1-' "$repo/calls/plan-from-spec" ||
    fail "replan did not fence generated ids"
  grep -Fq -- '--max-tasks 2' "$repo/calls/plan-from-spec" ||
    fail "replan task cap missing"
  grep -Fq -- '--allowed src,tests' "$repo/calls/plan-from-spec" ||
    fail "replan scope envelope missing"

  # A post-exhaustion planner rejection must close the proposing receipt rather
  # than stranding it. The exact empty-proposal diagnostic gets its own durable
  # reason; an unextractable planner answer remains a distinct generic failure.
  write_done_plan "$repo" false
  rc=0
  OMS_T_GOAL_RESULT=exhausted OMS_T_PLAN_RESULT=empty run_autopilot "$repo" run \
    --planner claude --worker codex --allowed 'src/,tests/' --base main \
    > "$repo/replan-empty.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "an empty replan should park, got $rc"
  grep -Fq 'plan-from-spec: proposal carries no tasks' "$repo/replan-empty.out" ||
    fail "the empty replan diagnostic was not preserved"
  grep -Fq 'reason=replan-empty' "$repo/replan-empty.out" ||
    fail "an empty replan parked for the wrong reason"
  grep -Fq 'the planner found nothing to add; inspect the acceptance failure and the planner artifact' \
    "$repo/replan-empty.out" || fail "the empty replan next action was missing"
  if ! python3 - "$repo/.oms/plan/autopilot-run.json" <<'PY'
import json, sys
assert json.load(open(sys.argv[1], encoding="utf-8"))["stage"] == "parked"
PY
  then
    fail "an empty replan did not leave a parked receipt"
  fi

  # A receipt copied into an unrelated checkout still names the original
  # execution checkout; inspection (the gate every resume passes) must not trust it there.
  local copied="$TMP/copied-receipt"
  make_repo "$copied"
  mkdir -p "$copied/.oms/plan" "$copied/calls"
  cp "$repo/.oms/plan/autopilot-run.json" "$copied/.oms/plan/autopilot-run.json"
  run_autopilot "$copied" status > "$copied/status.out" 2>&1 || true
  grep -Fq 'outer run: invalid receipt' "$copied/status.out" ||
    fail "a copied receipt validated in another checkout: $(cat "$copied/status.out")"
  # Nor may it abandon there: the receipt's checkout answers to another state root.
  local copied_before
  copied_before="$(checkout_state "$copied" "$repo")"
  rc=0
  run_autopilot "$copied" abandon --reason 'copied receipt' > "$copied/abandon.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] && grep -Fq 'belongs to a different repository' "$copied/abandon.out" ||
    fail "a copied receipt was abandoned in another checkout, got $rc: $(tail -3 "$copied/abandon.out")"
  [ "$copied_before" = "$(checkout_state "$copied" "$repo")" ] ||
    fail "a refused copied-receipt abandon changed HEAD, refs or .oms state"

  # A receipt that cannot prove its binding (a symlink to a FIFO, oversized
  # valid JSON, malformed JSON) refuses run, propose and reenter from the
  # canonical checkout and a live scratch alike: promptly, before any effect.
  local hostile="$TMP/hostile-receipt" hostile_managed="$TMP/hostile-managed"
  local hostile_scratch hostile_receipt hostile_kinds hostile_kind hostile_caller
  local hostile_action hostile_before
  make_repo "$hostile"
  mkdir -p "$hostile/calls" "$hostile_managed"
  hostile="$(cd "$hostile" && pwd -P)"
  write_done_plan "$hostile" false
  hostile_scratch="$(OMS_DELEGATE_WORKTREE_ROOT="$hostile_managed" \
    "$ROOT/scripts/scratch-worktree.sh" add --repo "$hostile" --owner-pid $$)" ||
    fail "hostile receipt scratch fixture creation failed"
  mkdir -p "$hostile_scratch/calls"
  hostile_receipt="$hostile/.oms/plan/autopilot-run.json"
  hostile_kinds="oversized malformed"
  if mkfifo "$TMP/hostile-fifo" 2>/dev/null &&
    ln -s "$TMP/hostile-fifo" "$TMP/hostile-link" 2>/dev/null && [ -L "$TMP/hostile-link" ]; then
    hostile_kinds="fifo $hostile_kinds"
  fi
  for hostile_kind in $hostile_kinds; do
    rm -f "$hostile_receipt"
    case "$hostile_kind" in
      fifo) ln -s "$TMP/hostile-fifo" "$hostile_receipt" ;;
      oversized)
        python3 - "$hostile_receipt" "$hostile" <<'PY' ;;
import json, sys
json.dump({"repo": sys.argv[2], "stage": "proposing", "pad": "x" * 70000},
          open(sys.argv[1], "w", encoding="utf-8"))
PY
      malformed) printf '{"repo": "%s", "stage": ' "$hostile" > "$hostile_receipt" ;;
    esac
    for hostile_caller in "$hostile" "$hostile_scratch"; do
      for hostile_action in run propose reenter; do
        hostile_before="$(checkout_state "$hostile" "$hostile_scratch")"
        rc=0
        OMS_DELEGATE_WORKTREE_ROOT="$hostile_managed" OMS_T_REPO="$hostile_caller" \
          OMS_T_CALLS="$hostile_caller/calls" OMS_T_STATE_REPO="$hostile" \
          OMS_AUTOPILOT_GOAL_DRIVE="$TMP/bin/goal-drive" \
          OMS_AUTOPILOT_PLAN_FROM_SPEC="$TMP/bin/plan-from-spec" \
          OMS_AUTOPILOT_PEER_REVIEW="$TMP/bin/peer-review" \
          OMS_AUTOPILOT_DRAFT_PR="$TMP/bin/draft-pr" \
          run_bounded "$hostile/hostile.out" "$ROOT/scripts/autopilot.sh" --repo "$hostile_caller" \
            "$hostile_action" --planner claude --worker codex --reviewer claude \
            --allowed 'src,tests' --base main || rc=$?
        [ "$rc" = 2 ] && grep -Fq 'cannot read the outer receipt binding' "$hostile/hostile.out" ||
          fail "$hostile_action from $hostile_caller over a $hostile_kind receipt must refuse at admission, got $rc: $(tail -5 "$hostile/hostile.out")"
        [ "$hostile_before" = "$(checkout_state "$hostile" "$hostile_scratch")" ] ||
          fail "$hostile_action from $hostile_caller over a $hostile_kind receipt changed HEAD, refs or .oms state"
      done
    done
  done
  rm -f "$hostile_receipt"
  # An absent receipt still gets the location check before any branch switch.
  local plan_real="$TMP/hostile-plan-real"
  git -C "$hostile" switch -q main
  mv "$hostile/.oms/plan" "$plan_real"
  ln -s "$plan_real" "$hostile/.oms/plan"
  hostile_before="$(checkout_state "$hostile" "$hostile_scratch")"
  rc=0
  run_autopilot "$hostile" run --planner claude --worker codex --reviewer claude \
    --allowed 'src,tests' --base main > "$hostile/hostile.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] && grep -Fq 'cannot bind this run to the durable outer receipt' "$hostile/hostile.out" ||
    fail "run over a symlinked plan directory must refuse before branch selection, got $rc: $(tail -5 "$hostile/hostile.out")"
  [ "$hostile_before" = "$(checkout_state "$hostile" "$hostile_scratch")" ] &&
    [ "$(git -C "$hostile" branch --show-current)" = main ] ||
    fail "refused run over a symlinked plan directory switched a branch"
  rm "$hostile/.oms/plan"
  mv "$plan_real" "$hostile/.oms/plan"
  # A receipt write that refuses after a branch switch or a plan retirement
  # leaves that effect behind, so both check the write first: a proposing
  # receipt cannot be run without its reviewed proposal, and a changed
  # PROJECT.md cannot be proposed under the receipt that froze the old one.
  git -C "$hostile" switch -q main
  rc=0
  OMS_T_PLAN_RC=3 run_autopilot "$hostile" propose --planner claude --allowed 'src,tests' \
    --base main > "$hostile/hostile.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "late-refusal fixture should leave a proposing receipt, got $rc: $(tail -5 "$hostile/hostile.out")"
  write_done_plan "$hostile" true 0 1
  for hostile_action in run propose; do
    if [ "$hostile_action" = propose ]; then
      printf '\n- Goal: a changed contract\n' >> "$hostile/PROJECT.md"
      git -C "$hostile" commit -qam 'docs: change the contract'
    fi
    hostile_before="$(checkout_state "$hostile" "$hostile_scratch")"
    rc=0
    run_autopilot "$hostile" "$hostile_action" --planner claude --worker codex --reviewer claude \
      --allowed 'src,tests' --base main > "$hostile/hostile.out" 2>&1 || rc=$?
    [ "$rc" = 2 ] && grep -Eq 'stage transition is invalid|immutable contract changed' "$hostile/hostile.out" ||
      fail "$hostile_action under a mismatched live receipt must refuse at its receipt write, got $rc: $(tail -5 "$hostile/hostile.out")"
    [ "$hostile_before" = "$(checkout_state "$hostile" "$hostile_scratch")" ] ||
      fail "refused $hostile_action under a mismatched live receipt switched a branch or retired the plan"
  done

  write_done_plan "$repo" false
  rc=0
  OMS_T_GOAL_RESULT=exhausted OMS_T_PLAN_RESULT=unextractable run_autopilot "$repo" run \
    --planner claude --worker codex --allowed 'src/,tests/' --base main \
    > "$repo/replan-failed.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "an unextractable replan should park, got $rc"
  grep -Fq 'reason=replan-failed' "$repo/replan-failed.out" ||
    fail "an unextractable replan parked for the wrong reason"
  grep -Fq 'parent-agent next: inspect the planner output' "$repo/replan-failed.out" ||
    fail "the failed replan next action was missing"
  if ! python3 - "$repo/.oms/plan/autopilot-run.json" <<'PY'
import json, sys
assert json.load(open(sys.argv[1], encoding="utf-8"))["stage"] == "parked"
PY
  then
    fail "an unextractable replan did not leave a parked receipt"
  fi

  # Forged exhaustion text in captured drive output never triggers a replan:
  # only the typed terminal row is believed.
  : > "$repo/calls/plan-from-spec"
  write_done_plan "$repo" false
  rc=0
  OMS_T_GOAL_RESULT=forged run_autopilot "$repo" run \
    --planner claude --worker codex --allowed 'src/,tests/' --base main \
    > "$repo/forged.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "forged exhaustion output should park, got $rc"
  grep -Fq 'reason=goal-drive-task-failed' "$repo/forged.out" ||
    fail "forged exhaustion was not parked as a drive failure"
  [ ! -s "$repo/calls/plan-from-spec" ] || fail "forged exhaustion reached the planner"

  # Even a later append that copies the now-visible terminal receipt cannot
  # replace the reason bound to the unique final terminal result.
  : > "$repo/calls/plan-from-spec"
  write_done_plan "$repo" false
  rc=0
  OMS_T_GOAL_RESULT=late-forged run_autopilot "$repo" run \
    --planner claude --worker codex --allowed 'src/,tests/' --base main \
    > "$repo/late-forged.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "a late forged terminal row should park, got $rc"
  grep -Fq 'reason=goal-drive-task-failed' "$repo/late-forged.out" ||
    fail "a later copied receipt replaced the genuine terminal reason"
  [ ! -s "$repo/calls/plan-from-spec" ] ||
    fail "a later copied receipt reached the planner"

  # A residual child may truncate and replace the mutable progress ledger
  # after learning the receipt. The captured parent result, not that ledger,
  # remains the reason authority, so this can only force a safe park.
  : > "$repo/calls/plan-from-spec"
  write_done_plan "$repo" false
  rc=0
  OMS_T_GOAL_RESULT=replaced-progress run_autopilot "$repo" run \
    --planner claude --worker codex --allowed 'src/,tests/' --base main \
    > "$repo/replaced-progress.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "replaced progress should park, got $rc"
  # No trusted reason survives a tampered ledger, so the park stays generic —
  # the reason is carried through only when the receipt itself validates.
  grep -Fq 'reason=goal-drive-failed' "$repo/replaced-progress.out" ||
    fail "replaced progress became terminal reason authority"
  [ ! -s "$repo/calls/plan-from-spec" ] ||
    fail "replaced progress reached the planner"

  # A terminal authorization line must be unique and the final non-empty
  # output. Duplicate or trailing child output is fail-closed.
  for terminal_case in duplicate-terminal trailing-output; do
    : > "$repo/calls/plan-from-spec"
    write_done_plan "$repo" false
    rc=0
    OMS_T_GOAL_RESULT="$terminal_case" run_autopilot "$repo" run \
      --planner claude --worker codex --allowed 'src/,tests/' --base main \
      > "$repo/$terminal_case.out" 2>&1 || rc=$?
    [ "$rc" = 3 ] || fail "$terminal_case should park, got $rc"
    [ ! -s "$repo/calls/plan-from-spec" ] ||
      fail "$terminal_case reached the planner"
  done

  # A terminal row from an earlier invocation is not evidence for this drive.
  # If the current goal-drive exits 3 without durably appending its own row,
  # autopilot must fail closed instead of spending the r1 proposal tranche.
  printf '%s\n' \
    '{"schema":1,"kind":"terminal","run_id":"old","status":"park","reason":"tasks-exhausted"}' \
    > "$repo/.oms/plan/progress.jsonl"
  : > "$repo/calls/plan-from-spec"
  write_done_plan "$repo" false
  rc=0
  OMS_T_GOAL_RESULT=no-terminal run_autopilot "$repo" run \
    --planner claude --worker codex --allowed 'src/,tests/' --base main \
    > "$repo/stale-terminal.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "a stale terminal row should not trigger exit 4, got $rc"
  grep -Fq 'reason=goal-drive-failed' "$repo/stale-terminal.out" ||
    fail "a missing current terminal row did not fail closed"
  [ ! -s "$repo/calls/plan-from-spec" ] ||
    fail "a stale tasks-exhausted row reached the planner"

  # Exit zero is not completion authority by itself. The outer orchestrator
  # requires goal-drive's unique terminal-v1 line and matching durable row.
  write_done_plan "$repo" true
  rc=0
  OMS_T_GOAL_RESULT=success-no-terminal run_autopilot "$repo" run \
    --planner claude --worker codex --allowed 'src/,tests/' --base main \
    > "$repo/no-success-terminal.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "success without a durable terminal receipt should park, got $rc"
  grep -Fq 'reason=goal-drive-result-invalid' "$repo/no-success-terminal.out" ||
    fail "missing successful drive receipt parked for the wrong reason"

  # status survives a vanished or dangling proposal file.
  ln -s missing-target "$repo/.oms/plan/proposal-dangling.json"
  run_autopilot "$repo" status > "$repo/status.out" 2>&1 ||
    fail "status crashed on a dangling proposal: $(tail -3 "$repo/status.out")"
  rm -f "$repo/.oms/plan/proposal-dangling.json"

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

  # A plan that no autopilot contract binds is not a plan for this contract.
  # `intent adopt` ends by naming plan-from-spec as a next step, and the plan
  # that writes carries no project_contract at all, so the very next
  # `autopilot propose` used to skip generation and die inside the binder with
  # "approved plan lacks a valid PROJECT.md/scope contract" -- naming neither
  # the leftover plan nor what to do about it.
  local unbound_repo="$TMP/unbound-plan"
  make_repo "$unbound_repo"
  mkdir -p "$unbound_repo/calls" "$unbound_repo/.oms/plan"
  printf '{"schema":3,"goal":"a previous contract","accept":"true","tasks":{}}\n' \
    > "$unbound_repo/.oms/plan/tasks.json"
  rc=0
  run_autopilot "$unbound_repo" propose --planner claude --allowed 'src,tests' \
    --base main --provider-timeout 10m > "$unbound_repo/unbound.out" 2>&1 || rc=$?
  [ "$rc" = 4 ] ||
    fail "an unbound plan must not block the first proposal, got $rc: $(tail -5 "$unbound_repo/unbound.out")"
  ! grep -Fq 'lacks a valid PROJECT.md/scope contract' "$unbound_repo/unbound.out" ||
    fail "the binder error surfaced instead of a proposal: $(tail -5 "$unbound_repo/unbound.out")"

  # Unfinished work from a prior, contract-bound project is refused, not
  # overwritten, and the refusal says which plan and why.
  local busy_repo="$TMP/foreign-plan-busy"
  make_repo "$busy_repo"
  mkdir -p "$busy_repo/calls"
  write_done_plan "$busy_repo" false
  python3 - "$busy_repo/.oms/plan/tasks.json" <<'PY'
import json, sys

path = sys.argv[1]
with open(path, encoding="utf-8") as handle:
    plan = json.load(handle)
plan["tasks"]["t1"]["state"] = "ready"
with open(path, "w", encoding="utf-8") as handle:
    json.dump(plan, handle)
PY
  printf '\n## Decisions\n\n- Contract revision: successor.\n' >> "$busy_repo/PROJECT.md"
  git -C "$busy_repo" add PROJECT.md
  git -C "$busy_repo" commit -qm 'docs: adopt successor contract'
  rc=0
  run_autopilot "$busy_repo" propose --planner claude --allowed 'src,tests' \
    --base main --provider-timeout 10m > "$busy_repo/busy.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] ||
    fail "unfinished work under another contract should be a contract error, got $rc"
  grep -Fq 'still has unfinished tasks' "$busy_repo/busy.out" ||
    fail "the refusal must name the leftover plan: $(tail -3 "$busy_repo/busy.out")"
  grep -Fq 'oms autopilot abandon' "$busy_repo/busy.out" ||
    fail "the refusal must distinguish the outer receipt from the leftover plan: $(tail -3 "$busy_repo/busy.out")"
  [ ! -f "$busy_repo/calls/plan-from-spec" ] ||
    fail "a refused propose must not reach the planner"

  # A retirement veto must leave the old plan live and stop before either the
  # outer receipt or planner begins. A typed live marker is authority even
  # when the task state says done.
  local archive_failure_repo="$TMP/superseded-plan-propose-veto"
  make_repo "$archive_failure_repo"
  mkdir -p "$archive_failure_repo/calls" "$archive_failure_repo/.oms/delegations"
  write_done_plan "$archive_failure_repo" false
  (
    . "$ROOT/scripts/lib/file-lock.sh"
    native_pid="$(oms_process_native_pid "$$")"
    native_source="$(oms_process_native_pid_source)"
    printf '{"schema":4,"id":"live","pid":%s,"native_pid":%s,"native_pid_source":"%s","task_id":"t1","lease_id":"","executor_id":""}\n' \
      "$$" "$native_pid" "$native_source" \
      > "$archive_failure_repo/.oms/delegations/live.json"
  )
  printf '\n## Decisions\n\n- Contract revision: successor.\n' >> "$archive_failure_repo/PROJECT.md"
  git -C "$archive_failure_repo" add PROJECT.md
  git -C "$archive_failure_repo" commit -qm 'docs: adopt successor contract'
  rc=0
  run_autopilot "$archive_failure_repo" propose --planner claude \
    --allowed 'src,tests' --base main > "$archive_failure_repo/veto.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] ||
    fail "a failed superseded-plan retirement should refuse propose, got $rc"
  grep -Fq 'cannot preserve the superseded plan' "$archive_failure_repo/veto.out" ||
    fail "the retirement veto did not preserve its cause: $(tail -5 "$archive_failure_repo/veto.out")"
  [ -f "$archive_failure_repo/.oms/plan/tasks.json" ] ||
    fail "a refused superseded-plan retirement removed the live plan"
  [ ! -e "$archive_failure_repo/.oms/plan/retirements.jsonl" ] ||
    fail "a refused superseded-plan retirement wrote retirement authority"
  [ ! -e "$archive_failure_repo/.oms/plan/autopilot-run.json" ] ||
    fail "a retirement veto bound an outer receipt before retirement"
  [ ! -e "$archive_failure_repo/calls/plan-from-spec" ] ||
    fail "a retirement veto reached the planner"

  # Retire a completed plan from the prior contract before asking the planner
  # about its successor. The real planner sees existing done tasks and may use
  # them as dependencies, so generating first and retiring only at run leaves
  # an initial proposal that cannot apply after its predecessor disappears.
  local successor_repo="$TMP/superseded-plan-propose"
  local successor_bin="$TMP/superseded-plan-propose-bin"
  local successor_home="$TMP/superseded-plan-propose-home"
  local successor_digest successor_proposal successor_sha
  make_repo "$successor_repo"
  mkdir -p "$successor_repo/calls" "$successor_bin" "$successor_home"
  write_done_plan "$successor_repo" false
  successor_digest="$(sha256_file "$successor_repo/.oms/plan/tasks.json")"
  cat > "$successor_bin/codex" <<'EOF'
#!/usr/bin/env bash
cat >/dev/null
printf '%s\n' '{"tasks":[{"id":"t1","title":"feat: successor core","allowed":["src/"],"verify":"true","depends":[]},{"id":"t2","title":"test: successor coverage","allowed":["tests/"],"verify":"true","depends":["t1"]}]}'
EOF
  chmod +x "$successor_bin/codex"
  git -C "$successor_repo" switch -q main
  printf '\n## Decisions\n\n- Contract revision: successor.\n' >> "$successor_repo/PROJECT.md"
  git -C "$successor_repo" add PROJECT.md
  git -C "$successor_repo" commit -qm 'docs: adopt successor contract'
  git -C "$successor_repo" switch -qc \
    "oms/autopilot-$(sha256_file "$successor_repo/PROJECT.md" | cut -c1-12)"
  rc=0
  HOME="$successor_home" NVM_DIR="$successor_home/.nvm" \
    PATH="$successor_bin:$PATH" OMS_T_REPO="$successor_repo" \
    OMS_T_CALLS="$successor_repo/calls" \
    OMS_AUTOPILOT_GOAL_DRIVE="$TMP/bin/goal-drive" \
    OMS_AUTOPILOT_PLAN_FROM_SPEC="$ROOT/scripts/plan-from-spec.sh" \
    OMS_AUTOPILOT_PEER_REVIEW="$TMP/bin/peer-review" \
    OMS_AUTOPILOT_DRAFT_PR="$TMP/bin/draft-pr" \
    "$ROOT/scripts/autopilot.sh" --repo "$successor_repo" propose \
      --planner codex --worker claude --allowed 'src,tests' --base main \
      --allow-verifier-change \
      > "$successor_repo/propose.out" 2>&1 || rc=$?
  [ "$rc" = 4 ] ||
    fail "a successor contract should produce a reviewed proposal, got $rc: $(tail -8 "$successor_repo/propose.out")"
  if grep -Fq 'note: verify reads tests, which this task may also change' "$successor_repo/propose.out"; then
    fail "contract verifier-change consent should silence the planner advisory: $(tail -8 "$successor_repo/propose.out")"
  fi
  [ -f "$successor_repo/.oms/plan/tasks.$successor_digest.archive.json" ] ||
    fail "propose must preserve the spent predecessor before planning"
  [ ! -f "$successor_repo/.oms/plan/tasks.json" ] ||
    fail "the spent predecessor remained live while successor planning ran"
  successor_proposal="$(find "$successor_repo/.oms/plan" -type f -name 'proposal-*.json' | sed -n '1p')"
  [ -n "$successor_proposal" ] || fail "successor proposal was not written"
  python3 - "$successor_proposal" <<'PY' || fail "successor proposal retained predecessor task ids"
import json, sys

with open(sys.argv[1], encoding="utf-8") as handle:
    tasks = json.load(handle)["tasks"]
ids = [task["id"] for task in tasks]
assert ids[0] == "t1", ids
assert all(dep in ids for task in tasks for dep in task["depends"]), tasks
PY
  successor_sha="$(sha256_file "$successor_proposal")"
  rc=0
  HOME="$successor_home" NVM_DIR="$successor_home/.nvm" \
    PATH="$successor_bin:$PATH" OMS_T_REPO="$successor_repo" \
    OMS_T_CALLS="$successor_repo/calls" OMS_T_GOAL_RESULT=success \
    OMS_AUTOPILOT_GOAL_DRIVE="$TMP/bin/goal-drive" \
    OMS_AUTOPILOT_PLAN_FROM_SPEC="$ROOT/scripts/plan-from-spec.sh" \
    OMS_AUTOPILOT_PEER_REVIEW="$TMP/bin/peer-review" \
    OMS_AUTOPILOT_DRAFT_PR="$TMP/bin/draft-pr" \
    "$ROOT/scripts/autopilot.sh" --repo "$successor_repo" run \
      --planner codex --worker claude --allowed 'src,tests' --base main \
      --allow-verifier-change \
      --proposal "$successor_proposal" --expected-proposal-sha256 "$successor_sha" \
      > "$successor_repo/run.out" 2>&1 || rc=$?
  [ "$rc" = 0 ] ||
    fail "the clean successor proposal should apply, got $rc: $(tail -8 "$successor_repo/run.out")"
  python3 - "$successor_repo/.oms/plan/tasks.json" <<'PY' || fail "successor tasks were not applied"
import json, sys

with open(sys.argv[1], encoding="utf-8") as handle:
    tasks = json.load(handle)["tasks"]
assert list(tasks) == ["t1", "t2"], tasks
assert tasks["t2"]["depends"] == ["t1"], tasks
PY

  # The advisory remains useful when no consent carries into the planner, and
  # the root-scope branch follows the same consent rule as a token match.
  local verifier_note_repo="$TMP/verifier-change-note"
  local verifier_note_bin="$TMP/verifier-change-note-bin"
  local verifier_note_home="$TMP/verifier-change-note-home"
  make_repo "$verifier_note_repo"
  mkdir -p "$verifier_note_bin" "$verifier_note_home"
  cat > "$verifier_note_bin/codex" <<'EOF'
#!/usr/bin/env bash
cat >/dev/null
printf '%s\n' '{"tasks":[{"id":"t1","title":"chore: repo-wide verification","allowed":["."],"verify":"true","depends":[]}]}'
EOF
  chmod +x "$verifier_note_bin/codex"
  HOME="$verifier_note_home" NVM_DIR="$verifier_note_home/.nvm" \
    PATH="$verifier_note_bin:$PATH" \
    "$ROOT/scripts/plan-from-spec.sh" --repo "$verifier_note_repo" --to codex \
      > "$verifier_note_repo/unconsented.out" 2>&1 ||
    fail "an unconsented planner proposal should succeed: $(tail -4 "$verifier_note_repo/unconsented.out")"
  grep -Fq 'note: verify reads ., which this task may also change' \
    "$verifier_note_repo/unconsented.out" ||
    fail "an unconsented repo-wide verifier should retain its advisory: $(cat "$verifier_note_repo/unconsented.out")"
  HOME="$verifier_note_home" NVM_DIR="$verifier_note_home/.nvm" \
    PATH="$verifier_note_bin:$PATH" \
    "$ROOT/scripts/plan-from-spec.sh" --repo "$verifier_note_repo" --to codex \
      --allow-verifier-change > "$verifier_note_repo/consented.out" 2>&1 ||
    fail "a consented planner proposal should succeed: $(tail -4 "$verifier_note_repo/consented.out")"
  if grep -Fq 'note: verify reads' "$verifier_note_repo/consented.out"; then
    fail "verifier-change consent should silence the repo-wide planner advisory: $(cat "$verifier_note_repo/consented.out")"
  fi

  # A live receipt freezes its contract, so a retry that changes one of those
  # fields is refused -- correctly. The refusal has to say which field and what
  # the way out is, or the reader cannot tell a decision from a bug: today's
  # field run hit it after the frozen planner timed out, and the only escape
  # was a verb the message never mentioned.
  local frozen_repo="$TMP/frozen-contract"
  make_repo "$frozen_repo"
  mkdir -p "$frozen_repo/calls"
  rc=0
  OMS_T_PLAN_RC=3 run_autopilot "$frozen_repo" propose --planner claude \
    --allowed 'src,tests' --base main > "$frozen_repo/frozen-first.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "the fixture needs a live receipt from a failed propose, got $rc"
  rc=0
  run_autopilot "$frozen_repo" propose --planner codex \
    --allowed 'src,tests' --base main > "$frozen_repo/frozen-retry.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "a changed frozen contract should be a contract error, got $rc"
  grep -Fq 'immutable contract changed (' "$frozen_repo/frozen-retry.out" ||
    fail "the refusal must name the changed field: $(tail -3 "$frozen_repo/frozen-retry.out")"
  grep -Fq 'autopilot abandon' "$frozen_repo/frozen-retry.out" ||
    fail "the refusal must name the way out: $(tail -3 "$frozen_repo/frozen-retry.out")"
  # The provenance line before the refusal prints --allowed by design; only the
  # refusal itself must stay value-free.
  ! grep -F 'immutable contract changed' "$frozen_repo/frozen-retry.out" | grep -F 'src,tests' > /dev/null ||
    fail "the refusal must name fields, never their values: $(tail -3 "$frozen_repo/frozen-retry.out")"

  # A reviewed proposal can go stale -- HEAD moves under it, or the parent
  # wants a different plan -- and the obvious next step is to propose again.
  # That edge is deliberately closed so a regenerated plan cannot slip under a
  # review that already happened, which leaves exactly one way forward; the
  # refusal has to name it.
  local reviewed_repo="$TMP/stale-reviewed-proposal"
  make_repo "$reviewed_repo"
  mkdir -p "$reviewed_repo/calls"
  rc=0
  run_autopilot "$reviewed_repo" propose --planner claude --allowed 'src,tests' \
    --base main > "$reviewed_repo/stale-first.out" 2>&1 || rc=$?
  [ "$rc" = 4 ] || fail "the fixture needs a receipt at proposal-review, got $rc: $(tail -3 "$reviewed_repo/stale-first.out")"
  rc=0
  run_autopilot "$reviewed_repo" propose --planner claude --allowed 'src,tests' \
    --base main > "$reviewed_repo/stale-retry.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "re-proposing over a reviewed proposal should be refused, got $rc"
  grep -Fq 'autopilot abandon' "$reviewed_repo/stale-retry.out" ||
    fail "the refusal must name the way out: $(tail -3 "$reviewed_repo/stale-retry.out")"

  # The apply side has the same question as propose: an existing plan is only
  # this contract's plan when the contract says so. plan-from-spec never
  # replaces a plan or its acceptance command, so a spent plan from a
  # superseded contract cannot receive the new tranche -- it is preserved
  # content-addressed and the new plan is created from PROJECT.md.
  local superseded_repo="$TMP/superseded-plan"
  make_repo "$superseded_repo"
  mkdir -p "$superseded_repo/calls" "$superseded_repo/.oms/plan"
  printf '{"schema":3,"goal":"a previous contract","accept":"true","tasks":{}}\n' \
    > "$superseded_repo/.oms/plan/tasks.json"
  local superseded_digest
  superseded_digest="$(sha256_file "$superseded_repo/.oms/plan/tasks.json")"
  cat > "$superseded_repo/.oms/plan/proposal-initial.json" <<'JSON'
{"schema":1,"kind":"agent-plan-proposal","id_prefix":"","tasks":[{"id":"t1","title":"feat: first","allowed":["src/"],"verify":"true","depends":[]}]}
JSON
  local superseded_sha
  superseded_sha="$(sha256_file "$superseded_repo/.oms/plan/proposal-initial.json")"
  rc=0
  OMS_T_GOAL_RESULT=success run_autopilot "$superseded_repo" run \
    --planner claude --worker codex --allowed 'src,tests' --base main \
    --proposal "$superseded_repo/.oms/plan/proposal-initial.json" \
    --expected-proposal-sha256 "$superseded_sha" \
    > "$superseded_repo/superseded.out" 2>&1 || rc=$?
  ! grep -Fq 'only the single r1- remainder' "$superseded_repo/superseded.out" ||
    fail "a superseded plan blocked this contract's first tranche: $(tail -5 "$superseded_repo/superseded.out")"
  [ -f "$superseded_repo/.oms/plan/tasks.$superseded_digest.archive.json" ] ||
    fail "the superseded plan must be preserved, not deleted: $(ls "$superseded_repo/.oms/plan")"
  grep -Fq '"acceptance_verified": false' \
    "$superseded_repo/.oms/plan/retirements.jsonl" ||
    fail "superseded retirement must not claim goal verification"
  grep -Fq -- '--apply' "$superseded_repo/calls/plan-from-spec" ||
    fail "the reviewed proposal never reached apply: $(tail -3 "$superseded_repo/superseded.out")"

  # If the typed retirement died after unlinking the old live plan but before
  # removing its intent, the next reviewed apply must finish that exact
  # completed transaction before it creates the replacement plan.
  local residual_repo="$TMP/superseded-plan-residual"
  local residual_digest residual_intent residual_proposal residual_sha
  make_repo "$residual_repo"
  mkdir -p "$residual_repo/calls" "$residual_repo/.oms/plan"
  printf '{"schema":3,"goal":"a previous contract","accept":"false","tasks":{}}\n' \
    > "$residual_repo/.oms/plan/tasks.json"
  residual_digest="$(sha256_file "$residual_repo/.oms/plan/tasks.json")"
  "$ROOT/scripts/agent-plan.sh" --repo "$residual_repo" retire --apply \
    --expected-plan-sha256 "$residual_digest" --disposition superseded \
    --reason "a reviewed PROJECT.md contract superseded this completed plan" >/dev/null
  residual_intent="$residual_repo/.oms/plan/tasks.$residual_digest.retire-intent.json"
  python3 - "$residual_repo/.oms/plan/retirements.jsonl" "$residual_intent" <<'PY'
import json, sys
receipt = json.loads(open(sys.argv[1], encoding="utf-8").readline())
row = {
    "schema": 1, "kind": "plan-retirement-intent",
    "plan_sha256": receipt["plan_sha256"],
    "disposition": receipt["disposition"], "reason": receipt["reason"],
    "head": receipt["head"], "git_ref": receipt["git_ref"],
    "plan_generation": receipt["plan_generation"],
    "proof_id": receipt["proof_id"], "archive": receipt["archive"],
}
with open(sys.argv[2], "w", encoding="utf-8") as handle:
    handle.write(json.dumps(row, sort_keys=True) + "\n")
PY
  residual_proposal="$residual_repo/proposal-initial.json"
  write_proposal "$residual_proposal"
  mv "$residual_proposal" "$residual_repo/.oms/plan/proposal-initial.json"
  residual_proposal="$residual_repo/.oms/plan/proposal-initial.json"
  residual_sha="$(sha256_file "$residual_proposal")"
  rc=0
  OMS_T_REPO="$residual_repo" OMS_T_CALLS="$residual_repo/calls" \
    OMS_T_GOAL_RESULT=success \
    OMS_AUTOPILOT_GOAL_DRIVE="$TMP/bin/goal-drive" \
    OMS_AUTOPILOT_PLAN_FROM_SPEC="$ROOT/scripts/plan-from-spec.sh" \
    OMS_AUTOPILOT_PEER_REVIEW="$TMP/bin/peer-review" \
    OMS_AUTOPILOT_DRAFT_PR="$TMP/bin/draft-pr" \
    "$ROOT/scripts/autopilot.sh" --repo "$residual_repo" run \
      --planner claude --worker codex --allowed 'src,tests' --base main \
      --proposal "$residual_proposal" --expected-proposal-sha256 "$residual_sha" \
      > "$residual_repo/residual.out" 2>&1 || rc=$?
  [ "$rc" = 0 ] ||
    fail "autopilot did not recover a completed retirement before retry: $(tail -8 "$residual_repo/residual.out")"
  [ ! -e "$residual_intent" ] && [ -e "$residual_repo/.oms/plan/tasks.json" ] &&
    [ "$(wc -l < "$residual_repo/.oms/plan/retirements.jsonl" | tr -d ' ')" = 1 ] ||
    fail "autopilot retry left residual retirement state or lost the new plan"
  "$ROOT/scripts/state-verify.sh" --repo "$residual_repo" --json >/dev/null ||
    fail "autopilot completed-retirement recovery did not restore verified state"

  # Planner failure is a parked/error result, never a fake approval boundary.
  local planner_repo="$TMP/planner-failure"
  make_repo "$planner_repo"
  mkdir -p "$planner_repo/calls"
  rc=0
  OMS_T_PLAN_RC=3 run_autopilot "$planner_repo" propose --planner claude \
    --allowed 'src,tests' --base main > "$planner_repo/planner.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "planner failure should propagate, got $rc: $(tail -5 "$planner_repo/planner.out")"
  if grep -Fq 'awaits parent-agent review' "$planner_repo/planner.out"; then
    fail "planner failure was misreported as a proposal"
  fi
  run_autopilot "$planner_repo" status > "$planner_repo/planner-status.out" 2>&1 ||
    fail "planner interruption receipt should remain inspectable"
  grep -Fq 'outer run: stage=proposing' "$planner_repo/planner-status.out" ||
    fail "planner interruption did not retain its pre-call proposing receipt"
  grep -Eq '^  oms .* propose$' "$planner_repo/planner-status.out" ||
    fail "planner interruption status did not print an exact propose continuation"

  # The field scenario (2026-08-18): the frozen contract's own provider wall
  # killed the planner, and no amendment path existed. A changed contract
  # against the live receipt must still refuse — and abandon is the audited
  # append-only exit that lets a fresh propose bind a new contract without a
  # fresh clone.
  rc=0
  OMS_T_PLAN_RC=3 run_autopilot "$planner_repo" propose --planner claude \
    --allowed 'src,tests' --base main --provider-timeout 10m \
    > "$planner_repo/retry-changed.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "a changed contract against a live receipt must refuse, got $rc"
  grep -Fq 'immutable contract changed' "$planner_repo/retry-changed.out" ||
    fail "the refusal must name the immutable contract: $(tail -3 "$planner_repo/retry-changed.out")"
  # Abandon retires parent-owned authority. A delegated harness child may
  # inspect with status/shadow, but it cannot free the receipt for a new run.
  local child_receipt_before child_receipt_after
  local child_archives_before child_archives_after
  child_receipt_before="$(sha256_file "$planner_repo/.oms/plan/autopilot-run.json")"
  child_archives_before="$(find "$planner_repo/.oms/plan" -type f \
    -name 'autopilot-run.*.json' | wc -l | tr -d ' ')"
  rc=0
  OMS_HARNESS_CHILD=1 run_autopilot "$planner_repo" abandon \
    --reason "a child must not retire parent authority" \
    > "$planner_repo/child-abandon.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "a harness child abandoned parent authority, got $rc"
  child_receipt_after="$(sha256_file "$planner_repo/.oms/plan/autopilot-run.json")"
  child_archives_after="$(find "$planner_repo/.oms/plan" -type f \
    -name 'autopilot-run.*.json' | wc -l | tr -d ' ')"
  [ "$child_receipt_after" = "$child_receipt_before" ] ||
    fail "child abandon changed the live receipt"
  [ "$child_archives_after" = "$child_archives_before" ] ||
    fail "child abandon wrote an archive or abandon record"

  # Stage writers already serialize on the receipt lock. Abandon must use the
  # same lock around digest -> archive/record -> unlink, or it can delete a
  # newer atomic replacement after its CAS check.
  local abandon_lock_ready="$TMP/abandon-lock-ready"
  local abandon_lock_mutate="$TMP/abandon-lock-mutate"
  local abandon_lock_mutated="$TMP/abandon-lock-mutated"
  local abandon_lock_release="$TMP/abandon-lock-release"
  local abandon_lock_dir="$TMP/abandon-locks"
  local abandon_holder_pid abandon_pid abandon_status=0 mutated_receipt_sha
  cat > "$TMP/hold-autopilot-receipt-lock.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
. "$OMS_TEST_FILE_LOCK_HELPER"
hold_receipt_lock() {
  : > "$OMS_TEST_LOCK_READY"
  while [ ! -e "$OMS_TEST_LOCK_MUTATE" ]; do sleep 1; done
  python3 - "$OMS_TEST_RECEIPT" <<'PY'
import json
import os
import sys
import tempfile

path = sys.argv[1]
with open(path, encoding="utf-8") as handle:
    row = json.load(handle)
row["updated"] = "2099-01-01T00:00:00Z"
payload = (json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n").encode()
fd, temporary = tempfile.mkstemp(prefix=".autopilot-lock-test.", dir=os.path.dirname(path))
try:
    with os.fdopen(fd, "wb") as handle:
        handle.write(payload)
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(temporary, path)
finally:
    if os.path.exists(temporary):
        os.unlink(temporary)
PY
  : > "$OMS_TEST_LOCK_MUTATED"
  while [ ! -e "$OMS_TEST_LOCK_RELEASE" ]; do sleep 1; done
}
oms_with_file_lock "$OMS_TEST_RECEIPT" hold_receipt_lock
EOF
  OMS_LOCK_DIR="$abandon_lock_dir" \
    OMS_TEST_FILE_LOCK_HELPER="$ROOT/scripts/lib/file-lock.sh" \
    OMS_TEST_RECEIPT="$planner_repo/.oms/plan/autopilot-run.json" \
    OMS_TEST_LOCK_READY="$abandon_lock_ready" \
    OMS_TEST_LOCK_MUTATE="$abandon_lock_mutate" \
    OMS_TEST_LOCK_MUTATED="$abandon_lock_mutated" \
    OMS_TEST_LOCK_RELEASE="$abandon_lock_release" \
    bash "$TMP/hold-autopilot-receipt-lock.sh" &
  abandon_holder_pid=$!
  for _ in 1 2 3 4 5; do
    [ -e "$abandon_lock_ready" ] && break
    sleep 1
  done
  [ -e "$abandon_lock_ready" ] || {
    kill "$abandon_holder_pid" 2>/dev/null || true
    fail "receipt lock holder did not start"
  }
  OMS_LOCK_DIR="$abandon_lock_dir" run_autopilot "$planner_repo" abandon \
    --reason "provider wall killed the planner; rebinding with a longer wall" \
    > "$planner_repo/abandon.out" 2>&1 &
  abandon_pid=$!
  sleep 1
  if ! kill -0 "$abandon_pid" 2>/dev/null; then
    wait "$abandon_pid" || abandon_status=$?
    : > "$abandon_lock_mutate"
    : > "$abandon_lock_release"
    wait "$abandon_holder_pid" 2>/dev/null || true
    fail "abandon ignored the receipt lock (status $abandon_status)"
  fi
  if [ ! -f "$planner_repo/.oms/plan/autopilot-run.json" ]; then
    : > "$abandon_lock_mutate"
    : > "$abandon_lock_release"
    wait "$abandon_holder_pid" 2>/dev/null || true
    wait "$abandon_pid" 2>/dev/null || true
    fail "abandon removed the live receipt while its lock was held"
  fi
  : > "$abandon_lock_mutate"
  for _ in 1 2 3 4 5; do
    [ -e "$abandon_lock_mutated" ] && break
    sleep 1
  done
  [ -e "$abandon_lock_mutated" ] || {
    : > "$abandon_lock_release"
    wait "$abandon_holder_pid" 2>/dev/null || true
    wait "$abandon_pid" 2>/dev/null || true
    fail "locked stage-writer mutation did not complete"
  }
  mutated_receipt_sha="$(sha256_file "$planner_repo/.oms/plan/autopilot-run.json")"
  mutated_receipt_sha="${mutated_receipt_sha//$'\r'/}"
  : > "$abandon_lock_release"
  wait "$abandon_holder_pid" || fail "receipt lock holder failed"
  wait "$abandon_pid" || abandon_status=$?
  [ "$abandon_status" = 0 ] ||
    fail "abandon failed after the receipt lock released: $(cat "$planner_repo/abandon.out")"
  [ -s "$planner_repo/.oms/plan/autopilot-run.$mutated_receipt_sha.json" ] ||
    fail "abandon did not preserve the receipt version written under its lock"
  [ ! -f "$planner_repo/.oms/plan/autopilot-run.json" ] ||
    fail "the live receipt must be retired by abandon"
  ls "$planner_repo/.oms/plan/"autopilot-run.*.abandoned.json >/dev/null 2>&1 ||
    fail "the abandon record is missing"
  grep -Fq '"reason":"provider wall killed the planner' \
    "$planner_repo/.oms/plan/"autopilot-run.*.abandoned.json ||
    fail "the typed reason must ride the abandon record"
  grep -Fq '"predecessor":' \
    "$planner_repo/.oms/plan/"autopilot-run.*.abandoned.json ||
    fail "the abandon record must name its predecessor digest"
  archived_found=0
  for archived in "$planner_repo/.oms/plan/"autopilot-run.*.json; do
    case "$archived" in *.abandoned.json) continue ;; esac
    [ -s "$archived" ] && archived_found=1
  done
  [ "$archived_found" = 1 ] ||
    fail "the abandoned receipt's exact bytes must stay archived"
  rc=0
  run_autopilot "$planner_repo" propose --planner claude --allowed 'src,tests' \
    --base main --provider-timeout 10m > "$planner_repo/rebind.out" 2>&1 || rc=$?
  [ "$rc" = 4 ] ||
    fail "a fresh propose after abandon must bind the new contract, got $rc: $(tail -5 "$planner_repo/rebind.out")"

  # Contract-level verifier-change consent (round 24): the flag binds into
  # the receipt at intake, rides every continuation, reaches the drive, and
  # is immutable mid-run — widening landing authority requires abandon plus
  # a fresh contract, never a quiet flag on a later resume.
  local avc_repo="$TMP/verifier-consent"
  make_repo "$avc_repo"
  mkdir -p "$avc_repo/calls"
  write_done_plan "$avc_repo" false
  rc=0
  OMS_T_GOAL_RESULT=exhausted run_autopilot "$avc_repo" run \
    --planner claude --worker codex --allowed 'src/,tests/' --base main \
    --allow-verifier-change > "$avc_repo/avc.out" 2>&1 || rc=$?
  [ "$rc" = 4 ] || fail "consented run should still propose its replan, got $rc: $(tail -5 "$avc_repo/avc.out")"
  grep -Fq -- '--allow-verifier-change' "$avc_repo/calls/goal-drive" ||
    fail "verifier-change consent did not reach the drive"
  grep -Fq '"allow_verifier_change":true' "$avc_repo/.oms/plan/autopilot-run.json" ||
    fail "verifier-change consent is not bound in the receipt contract"
  grep -Fq -- '--allow-verifier-change' "$avc_repo/avc.out" ||
    fail "the printed continuation dropped the verifier-change consent"
  if ! python3 - "$avc_repo/.oms/plan/autopilot-reentries.jsonl" <<'PY'
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8")]
claims = [row for row in rows if row.get("kind") == "autopilot-drive-claim"]
assert claims
assert all(isinstance(row.get("claim_pid"), int) and row["claim_pid"] > 0
           for row in claims)
assert all(isinstance(row.get("claim_native_pid"), int)
           and row["claim_native_pid"] > 0 for row in claims)
PY
  then
    fail "drive claim did not persist its typed native process identity"
  fi
  rc=0
  OMS_T_GOAL_RESULT=exhausted run_autopilot "$avc_repo" run \
    --planner claude --worker codex --allowed 'src/,tests/' --base main \
    > "$avc_repo/avc-drop.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "dropping the consent mid-run must refuse, got $rc"
  grep -Fq 'immutable contract changed' "$avc_repo/avc-drop.out" ||
    fail "the consent drop refusal must name the immutable contract"
  # A receipt written before the field existed still loads and means "never
  # allowed" — stored user state gets tolerance, new writes always carry it.
  python3 - "$avc_repo/.oms/plan/autopilot-run.json" <<'PY' || fail "could not rewrite the receipt as its pre-field shape"
import json, sys
path = sys.argv[1]
with open(path, encoding="utf-8") as handle:
    row = json.load(handle)
row["contract"].pop("allow_verifier_change")
row.pop("owner_id")
with open(path, "w", encoding="utf-8") as handle:
    handle.write(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n")
PY
  run_autopilot "$avc_repo" status > "$avc_repo/avc-legacy.out" 2>&1 ||
    fail "a pre-field receipt must remain loadable: $(tail -3 "$avc_repo/avc-legacy.out")"
  if grep -Fq -- '--allow-verifier-change' "$avc_repo/avc-legacy.out"; then
    fail "a pre-field receipt must not invent verifier-change consent"
  fi

  # reenter (round 24, council unanimous implement-v1): the audited entrance
  # mirroring abandon's exit. A fresh session resumes the live run from the
  # receipt's own typed continuation; the typed reentry record lands before
  # any work, refusals are records too, and stages that would let re-entry
  # supply an approval refuse.
  local reenter_repo="$TMP/reenter-v1"
  local reenter_spec reenter_branch reenter_owner dead_pid dead_native_pid
  local current_native_pid
  make_repo "$reenter_repo"
  mkdir -p "$reenter_repo/calls"
  rc=0
  OMS_T_PLAN_RC=3 run_autopilot "$reenter_repo" propose --planner claude \
    --allowed 'src,tests' --base main > "$reenter_repo/re-seed.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "reenter seed should die at the planner, got $rc"
  # Digest drift refuses through the same gate every resume passes — and the
  # refusal is recorded before it is reported.
  printf '\nreenter drift\n' >> "$reenter_repo/PROJECT.md"
  rc=0
  run_autopilot "$reenter_repo" reenter > "$reenter_repo/reenter-drift.out" 2>&1 || rc=$?
  [ "$rc" != 0 ] || fail "digest drift must refuse re-entry"
  grep -Fq 'digest gate:' "$reenter_repo/.oms/plan/autopilot-reentries.jsonl" ||
    fail "the drift refusal must ride the reentry ledger"
  if ! python3 - "$reenter_repo/.oms/plan/autopilot-reentries.jsonl" <<'PY'
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8")]
claim = [row for row in rows if row.get("kind") == "autopilot-reenter"][-1]
assert isinstance(claim.get("claim_pid"), int) and claim["claim_pid"] > 0
assert isinstance(claim.get("claim_native_pid"), int) and claim["claim_native_pid"] > 0
PY
  then
    fail "reenter claim did not persist its typed native process identity"
  fi
  git -C "$reenter_repo" checkout -q -- PROJECT.md
  # Run-level single-flight (debate correction): claims key on the lineage
  # (spec+branch), never the receipt digest — a live-but-slow ORIGINAL drive
  # refuses re-entry even after its receipt moved. A dead claimant is
  # superseded, and its leases requeue before the drive resumes.
  reenter_spec="$(python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); print(r["spec_sha256"])' \
    "$reenter_repo/.oms/plan/autopilot-run.json")"
  reenter_branch="$(python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); print(r.get("branch",""))' \
    "$reenter_repo/.oms/plan/autopilot-run.json")"
  reenter_owner="$(python3 "$ROOT/scripts/lib/autopilot-receipt.py" owner-id \
    "$reenter_repo/.oms/plan/autopilot-run.json")"
  current_native_pid="$(
    # shellcheck source=scripts/lib/file-lock.sh
    . "$ROOT/scripts/lib/file-lock.sh"
    oms_process_native_pid "$$"
  )" || fail "could not resolve the live claim's native pid"
  current_native_pid="${current_native_pid//$'\r'/}"
  printf '{"kind":"autopilot-drive-claim","spec_sha256":"%s","branch":"%s","owner_id":"%s","claim_pid":%d,"claim_native_pid":%d,"schema":1}\n' \
    "$reenter_spec" "$reenter_branch" "$reenter_owner" "$$" "$current_native_pid" \
    >> "$reenter_repo/.oms/plan/autopilot-reentries.jsonl"
  rc=0
  run_autopilot "$reenter_repo" reenter > "$reenter_repo/reenter-claimed.out" 2>&1 || rc=$?
  [ "$rc" != 0 ] || fail "a live drive claim on the lineage must refuse re-entry"
  grep -Fq 'already held by a live session' "$reenter_repo/reenter-claimed.out" ||
    fail "the live-claim refusal must name the holder: $(tail -3 "$reenter_repo/reenter-claimed.out")"
  # shadow (raise-after-evidence protocol): the same decision sequence,
  # observed, never acted on — and observation always scans claims, so a
  # live holder is exactly what the evidence shows. One typed row per
  # observation on its own ledger; receipt and claim ledger untouched.
  local shadow_ledger="$reenter_repo/.oms/plan/autopilot-shadow.jsonl"
  local shadow_receipt_before shadow_claims_before
  shadow_receipt_before="$(python3 "$ROOT/scripts/lib/autopilot-receipt.py" digest \
    "$reenter_repo/.oms/plan/autopilot-run.json")"
  shadow_claims_before="$(wc -l < "$reenter_repo/.oms/plan/autopilot-reentries.jsonl")"
  rc=0
  run_autopilot "$reenter_repo" shadow --session smoke-live \
    > "$reenter_repo/shadow-live.out" 2>&1 || rc=$?
  [ "$rc" = 0 ] || fail "shadow must observe and exit 0, got $rc: $(tail -3 "$reenter_repo/shadow-live.out")"
  grep -Fq 'would-refuse' "$reenter_repo/shadow-live.out" ||
    fail "a live lineage claim must shadow as would-refuse: $(tail -3 "$reenter_repo/shadow-live.out")"
  grep -Fq '"live_claim_pid":' "$shadow_ledger" ||
    fail "the shadow row must name the live holder"
  grep -Fq '"session_id":"smoke-live"' "$shadow_ledger" ||
    fail "the shadow row must carry the observing session"
  sh -c 'python3 -c '\''import os; print(os.getppid())'\'' > "$1"; :' \
    _ "$TMP/reenter-dead-native-pid" &
  dead_pid=$!
  wait "$dead_pid" 2>/dev/null || true
  IFS= read -r dead_native_pid < "$TMP/reenter-dead-native-pid"
  rm -f "$TMP/reenter-dead-native-pid"
  dead_native_pid="${dead_native_pid//$'\r'/}"
  python3 - "$reenter_repo/.oms/plan/autopilot-reentries.jsonl" \
    "$dead_pid" "$dead_native_pid" <<'PY' || fail "could not rewrite the claim as stale"
import json, sys
path, dead, dead_native = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
rows = []
with open(path, encoding="utf-8") as handle:
    for line in handle:
        row = json.loads(line)
        if row.get("kind") == "autopilot-drive-claim":
            row["claim_pid"] = dead
            row["claim_native_pid"] = dead_native
        rows.append(row)
with open(path, "w", encoding="utf-8") as handle:
    for row in rows:
        handle.write(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n")
PY
  # The dead session leaves owned leases behind — the field drill's park
  # cause. Recovery must take only its markerless claim and exact dead worker;
  # another owner, a live worker, markerless running work, and review evidence
  # all remain held.
  python3 - "$reenter_repo/.oms/plan/tasks.json" "$reenter_repo/PROJECT.md" \
    "$reenter_owner" <<'PY' || fail "could not plant the leased plan"
import datetime, hashlib, json, sys
now = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")
spec = hashlib.sha256(open(sys.argv[2], "rb").read()).hexdigest()
owner = sys.argv[3]
other = "owner_22222222222222222222222222222222"
def task(task_id, state, lease, task_owner):
    return {
        "id": task_id, "title": "feat: " + task_id, "state": state,
        "depends": [], "allowed_paths": ["src/"], "forbidden_paths": [],
        "verify": "true", "role": "", "provider": "codex", "ttl": "",
        "artifact": "", "patch": "", "reason": "", "claimed_at": now,
        "executor_id": "", "executor_soul_sha256": "", "lease_epoch": 1,
        "lease_id": lease, "review_lease_id": lease if state == "review" else "",
        "repair_count": 0, "repair_artifact": "", "created": now, "updated": now,
        "autopilot_owner_id": task_owner,
    }
json.dump({
    "schema": 3, "goal": "finish", "accept": "true",
    "project_contract": {
        "schema": 1, "spec_sha256": spec,
        "allowed_envelope": ["src", "tests"],
        "acceptance_files": [], "acceptance_manifest": [],
    },
    "tasks": {
        "t9": task("t9", "claimed", "lease-dead-claim", owner),
        "t10": task("t10", "running", "lease-other", other),
        "t11": task("t11", "running", "lease-live", owner),
        "t12": task("t12", "running", "lease-dead-worker", owner),
        "t13": task("t13", "review", "lease-review", owner),
        "t14": task("t14", "running", "lease-markerless", owner),
    },
}, open(sys.argv[1], "w", encoding="utf-8"))
PY
  mkdir -p "$reenter_repo/.oms/delegations"
  printf '{"schema":4,"id":"live","pid":%d,"native_pid":%d,"task_id":"t11","lease_id":"lease-live","autopilot_owner_id":"%s"}\n' \
    "$$" "$current_native_pid" "$reenter_owner" > "$reenter_repo/.oms/delegations/live.json"
  printf '{"schema":4,"id":"dead","pid":%d,"native_pid":%d,"task_id":"t12","lease_id":"lease-dead-worker","autopilot_owner_id":"%s"}\n' \
    "$dead_pid" "$dead_native_pid" "$reenter_owner" > "$reenter_repo/.oms/delegations/dead.json"
  printf '{"schema":4,"id":"review","pid":%d,"native_pid":%d,"task_id":"t13","lease_id":"lease-review","autopilot_owner_id":"%s"}\n' \
    "$dead_pid" "$dead_native_pid" "$reenter_owner" > "$reenter_repo/.oms/delegations/review.json"
  # Shadow over the dead-claimant state, BEFORE the real reenter: the
  # verdict is would-resume naming the dead pid, and observation consumes
  # nothing — the lease stays claimed, no claim row lands, the receipt
  # digest holds — so the real reenter below still finds everything.
  rc=0
  run_autopilot "$reenter_repo" shadow --session smoke-dead \
    > "$reenter_repo/shadow-dead.out" 2>&1 || rc=$?
  [ "$rc" = 0 ] || fail "shadow over a dead claimant must exit 0, got $rc: $(tail -3 "$reenter_repo/shadow-dead.out")"
  grep -Fq 'would-resume' "$reenter_repo/shadow-dead.out" ||
    fail "a dead claimant must shadow as would-resume: $(tail -3 "$reenter_repo/shadow-dead.out")"
  grep -Fq 'oms autopilot reenter' "$reenter_repo/shadow-dead.out" ||
    fail "the would-resume line must name the verb to run"
  grep -Fq '"dead_claim_pid":' "$shadow_ledger" ||
    fail "the dead claimant is the evidence — the shadow row must name it"
  [ "$shadow_claims_before" = "$(wc -l < "$reenter_repo/.oms/plan/autopilot-reentries.jsonl")" ] ||
    fail "shadow observation must never write a claim"
  [ "$shadow_receipt_before" = "$(python3 "$ROOT/scripts/lib/autopilot-receipt.py" digest \
      "$reenter_repo/.oms/plan/autopilot-run.json")" ] ||
    fail "shadow observation must not move the receipt"
  [ "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["tasks"]["t9"]["state"])' \
      "$reenter_repo/.oms/plan/tasks.json")" = claimed ] ||
    fail "shadow observation must not requeue the dead session's lease"
  rc=0
  # Append an unrelated row immediately after the lock-protected reenter
  # helper returns. Recovery must consume the judgment token returned by that
  # same critical section, never reread the now-changed ambient ledger tail.
  local reenter_python_bin="$TMP/reenter-python-bin"
  local real_reenter_python
  real_reenter_python="$(command -v python3)"
  mkdir -p "$reenter_python_bin"
  cat > "$reenter_python_bin/python3" <<'EOF'
#!/usr/bin/env bash
set -o pipefail
if [ "${1:-}" = "$OMS_T_RECEIPT_HELPER" ] && [ "${2:-}" = reenter ]; then
  "$OMS_T_REAL_PYTHON" "$@" | sed 's/$/\r/'
  helper_rc=${PIPESTATUS[0]}
  if [ "$helper_rc" = 0 ]; then
    printf '{"schema":1,"kind":"autopilot-reenter-requeue","spec_sha256":"%s","branch":"%s","owner_id":"%s","requeued":["ambient"],"dead_pid":1,"superseded_stale_claim_pid":0,"updated":"2026-08-19T00:00:00Z"}\n' \
      "$OMS_T_REENTER_SPEC" "$OMS_T_REENTER_BRANCH" "$OMS_T_REENTER_OWNER" \
      >> "$OMS_T_REENTER_LEDGER"
  fi
  exit "$helper_rc"
fi
exec "$OMS_T_REAL_PYTHON" "$@"
EOF
  chmod +x "$reenter_python_bin/python3"
  OMS_T_REAL_PYTHON="$real_reenter_python" \
    OMS_T_RECEIPT_HELPER="$ROOT/scripts/lib/autopilot-receipt.py" \
    OMS_T_REENTER_LEDGER="$reenter_repo/.oms/plan/autopilot-reentries.jsonl" \
    OMS_T_REENTER_SPEC="$reenter_spec" OMS_T_REENTER_BRANCH="$reenter_branch" \
    OMS_T_REENTER_OWNER="$reenter_owner" \
    PATH="$reenter_python_bin:$PATH" \
    run_autopilot "$reenter_repo" reenter --reason "fresh session picks up the dead planner" \
      > "$reenter_repo/reenter.out" 2>&1 || rc=$?
  grep -Fq 'requeued task(s) t9,t12 held by dead session' "$reenter_repo/reenter.out" ||
    fail "the dead predecessor's lease was not requeued: $(tail -6 "$reenter_repo/reenter.out")"
  grep -Fq '"kind":"autopilot-reenter-requeue"' "$reenter_repo/.oms/plan/autopilot-reentries.jsonl" ||
    fail "the lease requeue must leave a typed record"
  [ "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["tasks"]["t9"]["state"])' \
      "$reenter_repo/.oms/plan/tasks.json")" = ready ] ||
    fail "the requeued task must be ready again"
  python3 - "$reenter_repo/.oms/plan/tasks.json" <<'PY' || fail "owner recovery stole a live, unrelated, or review task"
import json, sys
tasks = json.load(open(sys.argv[1], encoding="utf-8"))["tasks"]
assert tasks["t12"]["state"] == "ready"
assert tasks["t10"]["state"] == "running"
assert tasks["t11"]["state"] == "running"
assert tasks["t13"]["state"] == "review"
assert tasks["t14"]["state"] == "running"
PY
  # The resumed propose then refuses honestly: the requeued work is
  # actionable, so a remainder proposal is premature — run first.
  [ "$rc" = 2 ] ||
    fail "resume over an actionable plan should refuse the re-propose, got $rc: $(tail -5 "$reenter_repo/reenter.out")"
  grep -Fq 'unfinished tasks' "$reenter_repo/reenter.out" ||
    fail "the refusal must name the unfinished work: $(tail -3 "$reenter_repo/reenter.out")"
  # Fixture reset: with the leased-plan scenario proven, clear it so the
  # clean resume path below reaches the review boundary.
  rm -f "$reenter_repo/.oms/plan/tasks.json"
  rc=0
  run_autopilot "$reenter_repo" reenter --reason "fresh session, plan cleared" \
    > "$reenter_repo/reenter.out" 2>&1 || rc=$?
  [ "$rc" = 4 ] ||
    fail "re-entering a proposing receipt should re-run propose to review, got $rc: $(tail -5 "$reenter_repo/reenter.out")"
  grep -Fq 'awaits parent-agent review' "$reenter_repo/reenter.out" ||
    fail "the re-entered propose did not reach the review boundary"
  grep -Fq '"disposition":"resumed"' "$reenter_repo/.oms/plan/autopilot-reentries.jsonl" ||
    fail "the reentry record is missing its resumed disposition"
  grep -Fq '"stage_at_reentry":"proposing"' "$reenter_repo/.oms/plan/autopilot-reentries.jsonl" ||
    fail "the reentry record must name the stage it re-entered"
  grep -Fq '"predecessor":' "$reenter_repo/.oms/plan/autopilot-reentries.jsonl" ||
    fail "the reentry record must name its predecessor digest"
  grep -Fq '"superseded_stale_claim_pid":' "$reenter_repo/.oms/plan/autopilot-reentries.jsonl" ||
    fail "superseding a dead claim must be recorded, not silent"
  grep -Fq '"superseded_stale_claim_native_pid":' \
    "$reenter_repo/.oms/plan/autopilot-reentries.jsonl" ||
    fail "superseding a dead claim must retain its native process identity"
  # The receipt now awaits parent review: re-entry cannot supply the approval.
  rc=0
  run_autopilot "$reenter_repo" reenter > "$reenter_repo/reenter-review.out" 2>&1 || rc=$?
  [ "$rc" != 0 ] || fail "re-entering a proposal-review receipt must refuse"
  grep -Fq 'cannot supply the approval' "$reenter_repo/reenter-review.out" ||
    fail "the review-stage refusal must say why: $(tail -3 "$reenter_repo/reenter-review.out")"
  grep -Fq '"disposition":"refused"' "$reenter_repo/.oms/plan/autopilot-reentries.jsonl" ||
    fail "a refused reentry must still leave a typed record"
  # Shadow at the review boundary mirrors the same refusal as evidence.
  rc=0
  run_autopilot "$reenter_repo" shadow > "$reenter_repo/shadow-review.out" 2>&1 || rc=$?
  [ "$rc" = 0 ] || fail "shadow at proposal-review must exit 0, got $rc: $(tail -3 "$reenter_repo/shadow-review.out")"
  grep -Fq 'would-refuse: a proposal awaits parent review' "$reenter_repo/shadow-review.out" ||
    fail "the review-stage shadow verdict must say why: $(tail -3 "$reenter_repo/shadow-review.out")"
  grep -Fq '"verdict":"would-refuse"' "$shadow_ledger" ||
    fail "the would-refuse verdict must ride the shadow ledger"

  # A proposal continuation is executable, not a template with an unresolved
  # branch token. Collect the base before the planner is allowed to run; a
  # detached HEAD leaves no checked-out branch to default --base to.
  local missing_base_repo="$TMP/propose-missing-base"
  make_repo "$missing_base_repo"
  mkdir -p "$missing_base_repo/calls"
  git -C "$missing_base_repo" checkout -q --detach
  rc=0
  run_autopilot "$missing_base_repo" propose --allowed 'src,tests' \
    > "$missing_base_repo/missing-base.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "propose without --base should be a contract error, got $rc"
  grep -Fq 'propose requires --base' "$missing_base_repo/missing-base.out" ||
    fail "missing proposal base was rejected for the wrong reason"
  [ ! -s "$missing_base_repo/calls/plan-from-spec" ] ||
    fail "a proposal without its continuation base reached the planner"
  # No receipt, nothing to observe: shadow is a silent no-op that leaves
  # no evidence ledger behind in a repo without a live run.
  rc=0
  run_autopilot "$missing_base_repo" shadow > "$missing_base_repo/shadow-none.out" 2>&1 || rc=$?
  [ "$rc" = 0 ] || fail "shadow without a receipt must be a silent no-op, got $rc"
  [ ! -s "$missing_base_repo/shadow-none.out" ] ||
    fail "shadow without a receipt must print nothing: $(tail -3 "$missing_base_repo/shadow-none.out")"
  [ ! -e "$missing_base_repo/.oms/plan/autopilot-shadow.jsonl" ] ||
    fail "no receipt, no evidence ledger"

  # A completed plan whose acceptance already passes needs no remainder.
  local accepted_repo="$TMP/already-accepted"
  make_repo "$accepted_repo"
  mkdir -p "$accepted_repo/calls"
  write_done_plan "$accepted_repo" true
  run_autopilot "$accepted_repo" propose --allowed 'src,tests' --base main \
    > "$accepted_repo/accepted.out" 2>&1 ||
    fail "already accepted plan should finish without another proposal: $(tail -8 "$accepted_repo/accepted.out")"
  [ ! -s "$accepted_repo/calls/plan-from-spec" ] ||
    fail "already accepted plan called the remainder planner"
  grep -Fq 'acceptance already passes' "$accepted_repo/accepted.out" ||
    fail "already accepted plan did not explain why no remainder was created"

  # The exit-4 receipt is executable argv, not prose. It must survive a repo
  # path with spaces and retain every effective option that shapes the resumed
  # run, including the proposal caps used by the atomic apply.
  local continuation_repo="$TMP/continuation repo"
  local continuation_script="$TMP/continuation-command.sh"
  local continuation_args="$TMP/continuation-args"
  local continuation_proposal continuation_sha
  make_repo "$continuation_repo"
  mkdir -p "$continuation_repo/calls"
  rc=0
  run_autopilot "$continuation_repo" propose \
    --planner antigravity --worker claude --reviewer codex \
    --remote upstream --allowed 'src,tests' --base main \
    --max-cycles 7 --initial-tasks 9 --replan-tasks 1 --auto-repair \
    --retry-known --provider-timeout 17m \
    --planner-model planner-model-x --planner-fallback-model planner-fallback-x \
    --planner-reasoning-effort low \
    --worker-model worker-model-x --worker-fallback-model worker-fallback-x \
    --worker-reasoning-effort high \
    --reviewer-model reviewer-model-x --reviewer-fallback-model reviewer-fallback-x \
    --reviewer-reasoning-effort medium \
    --review-mode gate --draft-pr > "$continuation_repo/propose.out" 2>&1 || rc=$?
  [ "$rc" = 4 ] || fail "continuation fixture should stop for review, got $rc"
  continuation_proposal="$continuation_repo/.oms/plan/proposal-r1.json"
  continuation_sha="$(sha256_file "$continuation_proposal")"
  sed -n '/^  oms autopilot /,$p' "$continuation_repo/propose.out" > "$continuation_script"
  [ -s "$continuation_script" ] || fail "proposal output omitted its continuation command"
  OMS_T_CONTINUATION_ARGS="$continuation_args" PATH="$TMP/bin:$PATH" \
    bash "$continuation_script" || fail "the printed continuation is not shell-safe"
  python3 - "$continuation_args" "$continuation_repo" "$continuation_proposal" \
    "$continuation_sha" <<'PY' || fail "the continuation lost effective run options"
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    args = [line.rstrip("\n") for line in handle]

def value(name):
    assert args.count(name) == 1, (name, args)
    offset = args.index(name)
    assert offset + 1 < len(args), (name, args)
    return args[offset + 1]

assert args[0] == "autopilot", args
assert args.count("run") == 1, args
assert value("--repo") == sys.argv[2], args
assert value("--proposal") == sys.argv[3], args
assert value("--expected-proposal-sha256") == sys.argv[4], args
assert value("--allowed") == "src,tests", args
assert value("--base") == "main", args
assert value("--planner") == "antigravity", args
assert value("--worker") == "claude", args
assert value("--reviewer") == "codex", args
assert value("--remote") == "upstream", args
assert value("--max-cycles") == "7", args
assert value("--initial-tasks") == "9", args
assert value("--replan-tasks") == "1", args
assert value("--review-mode") == "gate", args
assert value("--provider-timeout") == "17m", args
assert value("--planner-model") == "planner-model-x", args
assert value("--planner-fallback-model") == "planner-fallback-x", args
assert value("--planner-reasoning-effort") == "low", args
assert value("--worker-model") == "worker-model-x", args
assert value("--worker-fallback-model") == "worker-fallback-x", args
assert value("--worker-reasoning-effort") == "high", args
assert value("--reviewer-model") == "reviewer-model-x", args
assert value("--reviewer-fallback-model") == "reviewer-fallback-x", args
assert value("--reviewer-reasoning-effort") == "medium", args
assert args.count("--auto-repair") == 1, args
assert args.count("--retry-known") == 1, args
assert args.count("--draft-pr") == 1, args
PY

  # Status validates the outer receipt, its proposal bytes, frozen base, and
  # option-derived continuation before presenting executable recovery argv.
  run_autopilot "$continuation_repo" status > "$continuation_repo/status.out" 2>&1 ||
    fail "valid outer receipt should be visible in status"
  grep -Fq 'outer run: stage=proposal-review' "$continuation_repo/status.out" ||
    fail "status omitted the outer proposal-review stage"
  grep -Fq 'parent-agent continuation:' "$continuation_repo/status.out" ||
    fail "status should label recovery argv as parent-agent control"
  grep -Fq -- '--provider-timeout 17m' "$continuation_repo/status.out" ||
    fail "status did not reconstruct the bound outer continuation"
  rc=0
  run_autopilot "$continuation_repo" propose \
    --planner antigravity --worker claude --reviewer codex \
    --remote upstream --allowed 'src,tests' --base main --max-cycles 6 \
    --review-mode gate --draft-pr > "$continuation_repo/contract-drift.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "an in-progress outer contract rewrite should fail, got $rc"
  grep -Fq 'immutable contract changed' "$continuation_repo/contract-drift.out" ||
    fail "outer contract drift was rejected for the wrong reason"
  run_autopilot "$continuation_repo" status > "$continuation_repo/status-after-drift.out" 2>&1 ||
    fail "failed contract drift corrupted the outer receipt"
  grep -Fq -- '--provider-timeout 17m' "$continuation_repo/status-after-drift.out" ||
    fail "failed contract drift replaced the safe continuation"
  local receipt_path="$continuation_repo/.oms/plan/autopilot-run.json"
  local receipt_copy="$continuation_repo/.oms/plan/autopilot-run-copy.json"
  cp "$receipt_path" "$receipt_copy"
  rm -f "$receipt_path"
  ln -s "$(basename "$receipt_copy")" "$receipt_path"
  run_autopilot "$continuation_repo" status > "$continuation_repo/status-symlink.out" 2>&1 ||
    fail "status should diagnose an unsafe receipt without crashing"
  grep -Fq 'outer run: invalid receipt' "$continuation_repo/status-symlink.out" ||
    fail "status trusted a symlink outer receipt"
  if grep -Fq 'parent-agent continuation:' "$continuation_repo/status-symlink.out"; then
    fail "invalid outer receipt exposed a continuation"
  fi

  # Routing controls are bounded and validated before reaching any provider.
  local invalid_option_repo="$TMP/invalid-autopilot-options"
  make_repo "$invalid_option_repo"
  mkdir -p "$invalid_option_repo/calls"
  for invalid_args in \
    '--provider-timeout 0' \
    '--provider-timeout forever' \
    '--worker-reasoning-effort impossible'; do
    rc=0
    # The values above contain no shell metacharacters; word splitting here is
    # deliberate so each fixture exercises the public two-argument option.
    # shellcheck disable=SC2086
    run_autopilot "$invalid_option_repo" propose --allowed 'src,tests' --base main \
      $invalid_args > "$invalid_option_repo/invalid.out" 2>&1 || rc=$?
    [ "$rc" = 2 ] || fail "invalid option '$invalid_args' should exit 2, got $rc"
  done
  [ ! -s "$invalid_option_repo/calls/plan-from-spec" ] ||
    fail "invalid routing controls reached the planner"
  rc=0
  OMS_T_PLAN_RC=3 run_autopilot "$invalid_option_repo" propose \
    --allowed 'src,tests' --base main --max-cycles 10 \
    --worker-timeout 24h > "$invalid_option_repo/max-timeout.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] ||
    fail "documented max worker timeout/cycle composition should launch, got $rc"
  local max_worker_repo="$TMP/max-worker-phase-wall"
  make_repo "$max_worker_repo"
  mkdir -p "$max_worker_repo/calls"
  write_done_plan "$max_worker_repo" true
  OMS_T_GOAL_RESULT=success run_autopilot "$max_worker_repo" run \
    --worker codex --base main --max-cycles 10 --worker-timeout 24h \
    --auto-repair --review-mode off > "$max_worker_repo/max.out" 2>&1 ||
    fail "max cycles/timeout/repair composition should fit the outer supervisor"
  grep -Fq 'autopilot: done' "$max_worker_repo/max.out" ||
    fail "max worker phase composition did not reach completion"

  # A campaign is a day's largest unit of work, and it used to reach the Work
  # Journal only as its commits. The terminal now records itself, and asks the
  # parent for the judgment no record can derive. Both are fail-open.
  local journal_repo="$TMP/journal-terminal"
  make_repo "$journal_repo"
  mkdir -p "$journal_repo/calls"
  write_done_plan "$journal_repo" true
  OMS_T_GOAL_RESULT=success run_autopilot "$journal_repo" run \
    --worker codex --base main --review-mode off \
    > "$journal_repo/journal.out" 2>&1 ||
    fail "the journal terminal case should reach completion"
  grep -Fq 'autopilot: done' "$journal_repo/journal.out" ||
    fail "the journal terminal case did not reach completion"
  grep -Fq 'Autopilot campaign landed' \
    "$journal_repo/.oms/work-journal/events.jsonl" ||
    fail "a landed campaign left no Work Journal event"
  grep -Fq '"type":"autopilot"' \
    "$journal_repo/.oms/work-journal/events.jsonl" ||
    fail "the campaign event did not carry the autopilot source type"
  grep -Fq 'oms agent-task update --decision' "$journal_repo/journal.out" ||
    fail "a landed campaign did not ask the parent to record why"

  # Disabling the journal keeps the campaign result and drops both additions.
  local journal_off_repo="$TMP/journal-terminal-off"
  make_repo "$journal_off_repo"
  mkdir -p "$journal_off_repo/calls"
  write_done_plan "$journal_off_repo" true
  OMS_WORK_JOURNAL=0 OMS_T_GOAL_RESULT=success \
    run_autopilot "$journal_off_repo" run \
      --worker codex --base main --review-mode off \
      > "$journal_off_repo/journal.out" 2>&1 ||
    fail "a disabled journal should not change the campaign result"
  grep -Fq 'autopilot: done' "$journal_off_repo/journal.out" ||
    fail "a disabled journal changed the campaign terminal"
  [ ! -e "$journal_off_repo/.oms/work-journal" ] ||
    fail "a disabled journal still materialized campaign state"
  ! grep -Fq 'oms agent-task update --decision' "$journal_off_repo/journal.out" ||
    fail "a disabled journal still emitted the decision hint"

  local unsafe_start_repo="$TMP/unsafe-git-at-start"
  make_repo "$unsafe_start_repo"
  mkdir -p "$unsafe_start_repo/calls"
  git -C "$unsafe_start_repo" config diff.external \
    "sh -c ': > \"$unsafe_start_repo/calls/start-diff-fired\"; exit 1'"
  rc=0
  run_autopilot "$unsafe_start_repo" status \
    > "$unsafe_start_repo/unsafe.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "status should reject executable local Git config, got $rc"
  [ ! -e "$unsafe_start_repo/calls/start-diff-fired" ] ||
    fail "status executed a configured external diff while rejecting it"

  # Git can deliberately hide tracked working-tree changes from status/diff.
  # Neither skip-worktree (S) nor assume-unchanged (lowercase -v tag) is a
  # valid autonomous-run surface, even when the ordinary clean-tree check is
  # fooled by it.
  local hidden_mode hidden_repo
  for hidden_mode in assume-unchanged skip-worktree; do
    hidden_repo="$TMP/hidden-index-$hidden_mode"
    make_repo "$hidden_repo"
    mkdir -p "$hidden_repo/calls"
    write_done_plan "$hidden_repo" true
    if [ "$hidden_mode" = assume-unchanged ]; then
      git -C "$hidden_repo" update-index --assume-unchanged src/app.txt
    else
      git -C "$hidden_repo" update-index --skip-worktree src/app.txt
    fi
    printf 'hidden before autopilot\n' >> "$hidden_repo/src/app.txt"
    [ -z "$(git -C "$hidden_repo" status --porcelain)" ] ||
      fail "$hidden_mode fixture was not hidden from ordinary status"
    rc=0
    OMS_T_GOAL_RESULT=success run_autopilot "$hidden_repo" run --worker codex \
      --reviewer claude --base main --review-mode gate \
      > "$hidden_repo/hidden.out" 2>&1 || rc=$?
    [ "$rc" = 2 ] || fail "$hidden_mode should fail before drive, got $rc"
    grep -Fq 'skip-worktree/assume-unchanged flags are forbidden' \
      "$hidden_repo/hidden.out" || fail "$hidden_mode was rejected for the wrong reason"
    [ ! -s "$hidden_repo/calls/goal-drive" ] ||
      fail "$hidden_mode reached goal-drive"
  done

  # Unattended implementation forces the strict surface guard even when a
  # caller inherited opt-out values, and every role receives its effective
  # routing/timeout settings.
  local routing_repo="$TMP/routing-controls"
  make_repo "$routing_repo"
  mkdir -p "$routing_repo/calls"
  write_done_plan "$routing_repo" true
  OMS_WORKER_GUARD_STRICT=0 OMS_WORKER_GUARD_OFF=1 OMS_T_GOAL_RESULT=success \
    run_autopilot "$routing_repo" run --worker codex --reviewer claude \
      --allowed 'src,tests' --base main --provider-timeout 23s --retry-known \
      --max-cycles 2 \
      --worker-model worker-model-y --worker-reasoning-effort high \
      --reviewer-model reviewer-model-y --reviewer-reasoning-effort low \
      --review-mode gate > "$routing_repo/routing.out" 2>&1 ||
    fail "valid routing controls should reach the bounded children"
  grep -Fq 'strict=1 off=0 timeout=23s' "$routing_repo/calls/goal-drive-env" ||
    fail "goal-drive did not receive the forced strict guard and timeout"
  grep -Fq -- '--retry-known' "$routing_repo/calls/goal-drive" ||
    fail "goal-drive did not receive retry-known"
  contract_id="$(python3 - "$routing_repo/.oms/plan/autopilot-run.json" <<'PY'
import json, sys
print(json.load(open(sys.argv[1], encoding="utf-8"))["owner_id"])
PY
)"
  grep -Fq -- "--contract-id $contract_id" "$routing_repo/calls/goal-drive" ||
    fail "goal-drive did not receive the durable autopilot contract identity"
  grep -Fq -- '--max-cycles 2' "$routing_repo/calls/goal-drive" ||
    fail "fresh autopilot contract did not receive its full cycle budget"
  grep -Fq -- '--model worker-model-y' "$routing_repo/calls/goal-drive" ||
    fail "goal-drive did not receive the worker model"
  grep -Fq -- '--reasoning-effort high' "$routing_repo/calls/goal-drive" ||
    fail "goal-drive did not receive the worker reasoning effort"
  grep -Fq -- '--model reviewer-model-y' "$routing_repo/calls/peer-review" ||
    fail "peer-review did not receive the reviewer model"
  grep -Fq -- '--reasoning-effort low' "$routing_repo/calls/peer-review" ||
    fail "peer-review did not receive the reviewer reasoning effort"
  grep -Fq 'timeout=23s' "$routing_repo/calls/peer-review-env" ||
    fail "peer-review did not receive the provider timeout"

  # The real goal driver reconstructs prior contract use from durable rows.
  # A resumed invocation at the cap must park before entering task execution.
  local cycle_repo="$TMP/lifetime-cycle-cap"
  local cycle_contract=owner_22222222222222222222222222222222
  make_repo "$cycle_repo"
  write_done_plan "$cycle_repo" false
  mkdir -p "$cycle_repo/.oms/plan"
  python3 - "$cycle_repo/.oms/plan/progress.jsonl" "$cycle_contract" <<'PY'
import json, sys
path, contract = sys.argv[1:]
with open(path, "w", encoding="utf-8") as handle:
    for cycle in (1, 2):
        handle.write(json.dumps({"schema": 1, "kind": "autopilot-cycle",
            "contract_id": contract, "run_id": "interrupted", "cycle": cycle}) + "\n")
PY
  rc=0
  "$ROOT/scripts/goal-drive.sh" --repo "$cycle_repo" --max-cycles 2 \
    --contract-id "$cycle_contract" --run-id resumed \
    > "$cycle_repo/resume.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] ||
    fail "a contract at its lifetime cycle cap should park, got $rc: $(tail -5 "$cycle_repo/resume.out")"
  grep -Fq 'reason=cycles-exhausted' "$cycle_repo/resume.out" ||
    fail "a capped resume did not park as cycles-exhausted"
  grep -Fq 'already used 2 of 2 cycle(s) across resumes' "$cycle_repo/resume.out" ||
    fail "the exhaustion message omitted lifetime cycle usage"
  grep -Fq 'retire it with oms autopilot abandon --reason TEXT, then start a fresh contract' "$cycle_repo/resume.out" ||
    fail "the exhaustion message omitted the contract retirement path"
  if grep -Eiq 'plan-run|provider' "$cycle_repo/resume.out"; then
    fail "a capped resume reached task execution or provider launch"
  fi

  # A process may stop immediately after persisting a downstream stage. The
  # exact status continuation safely replays through approved/driving instead
  # of becoming an unusable receipt at reviewing or publishing.
  local interrupted_stage
  for interrupted_stage in reviewing publishing; do
    python3 - "$routing_repo/.oms/plan/autopilot-run.json" "$interrupted_stage" <<'PY'
import json, sys
path, stage = sys.argv[1:]
with open(path, encoding="utf-8") as handle:
    row = json.load(handle)
row["stage"] = stage
with open(path, "w", encoding="utf-8") as handle:
    json.dump(row, handle, sort_keys=True, separators=(",", ":"))
    handle.write("\n")
PY
    OMS_T_GOAL_RESULT=success run_autopilot "$routing_repo" run \
      --worker codex --reviewer claude --allowed 'src,tests' --base main \
      --provider-timeout 23s --retry-known --max-cycles 2 \
      --worker-model worker-model-y --worker-reasoning-effort high \
      --reviewer-model reviewer-model-y --reviewer-reasoning-effort low \
      --review-mode gate > "$routing_repo/resume-$interrupted_stage.out" 2>&1 ||
      fail "$interrupted_stage outer receipt could not replay its exact continuation"
    grep -Fq 'autopilot: done' "$routing_repo/resume-$interrupted_stage.out" ||
      fail "$interrupted_stage replay did not reach a new done receipt"
  done

  # A completed receipt archives before a new invocation; the next contract
  # therefore receives a new durable identity and its own full cycle budget.
  prior_contract_id="$(python3 - "$routing_repo/.oms/plan/autopilot-run.json" <<'PY'
import json, sys
print(json.load(open(sys.argv[1], encoding="utf-8"))["owner_id"])
PY
)"
  OMS_T_GOAL_RESULT=success run_autopilot "$routing_repo" run \
    --worker codex --reviewer claude --allowed 'src,tests' --base main \
    --provider-timeout 23s --retry-known --max-cycles 2 \
    --worker-model worker-model-y --worker-reasoning-effort high \
    --reviewer-model reviewer-model-y --reviewer-reasoning-effort low \
    --review-mode gate > "$routing_repo/fresh-contract.out" 2>&1 ||
    fail "a fresh contract after completion should run with a new budget"
  fresh_contract_id="$(python3 - "$routing_repo/.oms/plan/autopilot-run.json" <<'PY'
import json, sys
print(json.load(open(sys.argv[1], encoding="utf-8"))["owner_id"])
PY
)"
  [ "$fresh_contract_id" != "$prior_contract_id" ] ||
    fail "a fresh autopilot contract reused the previous contract identity"
  grep -Fq -- "--contract-id $fresh_contract_id" "$routing_repo/calls/goal-drive" ||
    fail "fresh contract identity did not reach goal-drive"
  grep -Fq -- '--max-cycles 2' "$routing_repo/calls/goal-drive" ||
    fail "fresh contract did not receive its full cycle budget"

  # Recheck the hidden-index surface after every provider boundary. A child
  # cannot earn completion by hiding a tracked mutation after startup.
  local hidden_drive_repo="$TMP/hidden-index-after-drive"
  make_repo "$hidden_drive_repo"
  mkdir -p "$hidden_drive_repo/calls"
  write_done_plan "$hidden_drive_repo" true
  rc=0
  OMS_T_GOAL_RESULT=success OMS_T_GOAL_ASSUME_UNCHANGED=1 \
    run_autopilot "$hidden_drive_repo" run --worker codex --reviewer claude \
      --base main --review-mode gate > "$hidden_drive_repo/hidden.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "post-drive assume-unchanged should park, got $rc"
  grep -Fq 'reason=hidden-index-flags-after-drive' "$hidden_drive_repo/hidden.out" ||
    fail "post-drive assume-unchanged parked for the wrong reason"
  [ ! -s "$hidden_drive_repo/calls/peer-review" ] ||
    fail "hidden post-drive mutation reached semantic review"

  local unsafe_git_repo="$TMP/unsafe-git-after-drive"
  make_repo "$unsafe_git_repo"
  mkdir -p "$unsafe_git_repo/calls"
  write_done_plan "$unsafe_git_repo" true
  rc=0
  OMS_T_GOAL_RESULT=success OMS_T_GOAL_INSTALL_FILTER=1 \
    run_autopilot "$unsafe_git_repo" run --worker codex --reviewer claude \
      --base main --review-mode gate > "$unsafe_git_repo/unsafe.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "post-drive executable filter config should park, got $rc"
  grep -Fq 'reason=unsafe-git-execution-config' "$unsafe_git_repo/unsafe.out" ||
    fail "post-drive executable filter config parked for the wrong reason"
  [ ! -e "$unsafe_git_repo/calls/filter-fired" ] ||
    fail "post-drive executable filter config was invoked"
  [ ! -s "$unsafe_git_repo/calls/peer-review" ] ||
    fail "post-drive executable filter config reached semantic review"

  local hidden_review_repo="$TMP/hidden-index-after-review"
  make_repo "$hidden_review_repo"
  mkdir -p "$hidden_review_repo/calls"
  write_done_plan "$hidden_review_repo" true
  rc=0
  OMS_T_GOAL_RESULT=success OMS_T_REVIEW_SKIP_WORKTREE=1 \
    run_autopilot "$hidden_review_repo" run --worker codex --reviewer claude \
      --base main --review-mode gate > "$hidden_review_repo/hidden.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "post-review skip-worktree should park, got $rc"
  grep -Fq 'reason=hidden-index-flags-after-review' "$hidden_review_repo/hidden.out" ||
    fail "post-review skip-worktree parked for the wrong reason"

  local unsafe_review_repo="$TMP/unsafe-git-after-review"
  make_repo "$unsafe_review_repo"
  mkdir -p "$unsafe_review_repo/calls"
  write_done_plan "$unsafe_review_repo" true
  rc=0
  OMS_T_GOAL_RESULT=success OMS_T_REVIEW_INSTALL_FSMONITOR=1 \
    run_autopilot "$unsafe_review_repo" run --worker codex --reviewer claude \
      --base main --review-mode gate > "$unsafe_review_repo/unsafe.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "post-review fsmonitor config should park, got $rc"
  grep -Fq 'reason=unsafe-git-execution-config' "$unsafe_review_repo/unsafe.out" ||
    fail "post-review fsmonitor config parked for the wrong reason"
  [ ! -e "$unsafe_review_repo/calls/fsmonitor-fired" ] ||
    fail "post-review fsmonitor config was invoked"

  # A completed receipt is durable history, not a permanent repo lock. After
  # the operator returns to the base and installs a new confirmed spec, a new
  # proposal archives the done receipt by digest and starts a fresh binding.
  local done_receipt_sha
  local done_receipt_stage
  if ! python3 - "$routing_repo/.oms/plan/autopilot-run.json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    assert json.load(handle)["stage"] == "done"
PY
  then
    fail "same-spec remainder fixture did not begin from a done outer receipt"
  fi
  done_receipt_sha="$(sha256_file "$routing_repo/.oms/plan/autopilot-run.json")"
  done_receipt_stage="$(python3 - "$routing_repo/.oms/plan/autopilot-run.json" <<'PY'
import json, sys
print(json.load(open(sys.argv[1], encoding="utf-8"))["stage"])
PY
)"
  printf 'before=%s stage=%s\n' "$done_receipt_sha" "$done_receipt_stage" \
    > "$routing_repo/same-spec-debug.out"
  write_done_plan "$routing_repo" false 0 1
  rc=0
  run_autopilot "$routing_repo" propose --allowed 'src,tests' --base main \
    --worker codex --reviewer claude --provider-timeout 23s --retry-known \
    --worker-model worker-model-y --worker-reasoning-effort high \
    --reviewer-model reviewer-model-y --reviewer-reasoning-effort low \
    --review-mode gate > "$routing_repo/same-spec-replan.out" 2>&1 || rc=$?
  [ "$rc" = 4 ] ||
    fail "same-spec acceptance regression should archive done and propose r1, got $rc"
  ls "$routing_repo/.oms/plan"/autopilot-run* 2>/dev/null | sed 's#.*/##' \
    >> "$routing_repo/same-spec-debug.out"
  [ -f "$routing_repo/.oms/plan/autopilot-run.$done_receipt_sha.json" ] ||
    fail "same-spec remainder did not archive its completed outer receipt: $(ls "$routing_repo/.oms/plan"/autopilot-run* 2>/dev/null | sed 's#.*/##' | tr '\n' ' ')"
  rm -f "$routing_repo/.oms/plan/autopilot-run.json" \
    "$routing_repo/.oms/plan/proposal-r1.json"
  write_done_plan "$routing_repo" true
  OMS_T_GOAL_RESULT=success run_autopilot "$routing_repo" run \
    --worker codex --reviewer claude --allowed 'src,tests' --base main \
    --provider-timeout 23s --retry-known \
    --worker-model worker-model-y --worker-reasoning-effort high \
    --reviewer-model reviewer-model-y --reviewer-reasoning-effort low \
    --review-mode gate > "$routing_repo/restore-done.out" 2>&1 ||
    fail "same-spec archive fixture could not restore a completed outer run"
  done_receipt_sha="$(sha256_file "$routing_repo/.oms/plan/autopilot-run.json")"
  git -C "$routing_repo" switch -q main
  printf '\n- Goal: next bounded feature\n' >> "$routing_repo/PROJECT.md"
  git -C "$routing_repo" add PROJECT.md
  git -C "$routing_repo" commit -qm 'docs: confirm the next project goal'
  rm -f "$routing_repo/.oms/plan/tasks.json" \
    "$routing_repo/.oms/plan/progress.jsonl"
  rc=0
  run_autopilot "$routing_repo" propose --allowed 'src,tests' --base main \
    > "$routing_repo/next-propose.out" 2>&1 || rc=$?
  [ "$rc" = 4 ] || fail "new spec after a done run should reach proposal review, got $rc"
  [ -f "$routing_repo/.oms/plan/autopilot-run.$done_receipt_sha.json" ] ||
    fail "done outer receipt was not archived by digest"
  run_autopilot "$routing_repo" status > "$routing_repo/next-status.out" 2>&1 ||
    fail "new outer proposal receipt should validate"
  grep -Fq 'outer run: stage=proposal-review' "$routing_repo/next-status.out" ||
    fail "new confirmed spec did not start a fresh outer run"

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

  # Draft publication is strict by default. Shadow remains an explicit local
  # operator choice, but is never silently selected merely because no review
  # mode flag was written.
  write_done_plan "$repo" true
  : > "$repo/calls/peer-review"
  : > "$repo/calls/draft-pr"
  rc=0
  OMS_T_GOAL_RESULT=success OMS_T_REVIEW_RC=1 run_autopilot "$repo" run \
    --planner claude --worker codex --reviewer claude --allowed 'src/,tests/' \
    --base main --draft-pr > "$repo/default-gate.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "Draft PR should default to semantic gate, got $rc"
  [ ! -s "$repo/calls/draft-pr" ] || fail "default semantic gate reached Draft PR"

  write_done_plan "$repo" true
  : > "$repo/calls/peer-review"
  : > "$repo/calls/draft-pr"
  : > "$repo/calls/draft-pr-env"
  OMS_T_GOAL_RESULT=success OMS_T_REVIEW_RC=1 run_autopilot "$repo" run \
    --planner claude --worker codex --reviewer claude --allowed 'src/,tests/' \
    --base main --provider-timeout 1s --draft-pr --review-mode shadow \
    > "$repo/shadow.out" 2>&1 ||
    fail "explicit shadow semantic finding should remain advisory"
  grep -Fq 'semantic review: advisory fail' "$repo/shadow.out" ||
    fail "shadow review failure was hidden"
  grep -Fq 'prepare --repo' "$repo/calls/draft-pr" ||
    fail "draft intent was not prepared: $(cat "$repo/calls/draft-pr")"
  grep -Fq 'publish --repo' "$repo/calls/draft-pr" ||
    fail "draft intent was not published: $(cat "$repo/calls/draft-pr")"
  grep -Fq -- '--review-evidence mode=shadow outcome=advisory-fail reviewer=claude' \
    "$repo/calls/draft-pr" ||
    fail "the advisory review outcome was not disclosed to prepare"
  grep -Fq 'phase=prepare wall=86400' "$repo/calls/draft-pr-env" ||
    fail "tiny provider timeout prematurely narrowed the draft prepare ceiling"
  grep -Fq 'phase=publish wall=86400' "$repo/calls/draft-pr-env" ||
    fail "tiny provider timeout prematurely narrowed the draft publish ceiling"

  : > "$repo/calls/draft-pr"
  rc=0
  OMS_T_GOAL_RESULT=success OMS_T_REVIEW_RC=1 run_autopilot "$repo" run \
    --planner claude --worker codex --reviewer claude --allowed 'src/,tests/' \
    --base main --draft-pr --review-mode gate > "$repo/gate.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "strict semantic gate should park, got $rc"
  [ ! -s "$repo/calls/draft-pr" ] || fail "failed semantic gate reached remote publish"
  python3 - "$repo/.oms/plan/autopilot-run.json" "$repo" \
    "$ROOT/scripts/lib" <<'PY'
import json, os, runpy, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    receipt = json.load(handle)
diagnostic = receipt.get("diagnostic", {})
assert receipt["stage"] == "parked", receipt
assert diagnostic.get("phase") == "semantic-review", diagnostic
assert diagnostic.get("reason_code") == "semantic-review-failed", diagnostic
assert diagnostic.get("reason") == "semantic review rejected the accepted drive", diagnostic
assert diagnostic.get("semantic_verdict") == "fail", diagnostic
evidence = diagnostic.get("evidence_path", "")
assert evidence.startswith(os.path.realpath(sys.argv[2]) + os.sep), diagnostic
assert os.path.isfile(evidence), diagnostic
sys.path.insert(0, sys.argv[3])
rules = runpy.run_path(sys.argv[3] + "/autopilot-receipt.py")
rules["validate_receipt"](receipt)
nonparked = dict(receipt, stage="reviewing")
try:
    rules["validate_receipt"](nonparked)
except rules["ReceiptError"]:
    pass
else:
    raise AssertionError("a non-parked outer receipt accepted a diagnostic")
PY
  run_autopilot "$repo" status > "$repo/gate-status.out" 2>&1 ||
    fail "parked semantic campaign should remain visible in status"
  grep -Fq 'outer run: stage=parked' "$repo/gate-status.out" ||
    fail "status omitted parked outer stage"
  grep -Fq 'reason=semantic-review-failed' "$repo/gate-status.out" ||
    fail "status omitted semantic park reason"
  grep -Fq 'semantic=fail' "$repo/gate-status.out" ||
    fail "status omitted semantic verdict"
  grep -Fq 'semantic review rejected the accepted drive' "$repo/gate-status.out" ||
    fail "status did not show the park reason separately from the recovery hint"
  grep -Fq -- 'autopilot-review-' "$repo/gate-status.out" ||
    fail "status omitted semantic review evidence path"
  parked_line="$(grep -n -F 'outer run: stage=parked' "$repo/gate-status.out" | cut -d: -f1)"
  drive_line="$(grep -n -F 'latest drive: done (acceptance-pass)' "$repo/gate-status.out" | cut -d: -f1)"
  [ -n "$parked_line" ] && [ -n "$drive_line" ] &&
    [ "$parked_line" -lt "$drive_line" ] ||
    fail "status showed the earlier successful drive before the parked campaign"

  write_done_plan "$repo" true
  : > "$repo/calls/peer-review"
  rc=0
  OMS_T_GOAL_RESULT=success OMS_T_REVIEW_RC=2 run_autopilot "$repo" run \
    --planner claude --worker codex --reviewer claude --allowed 'src/,tests/' \
    --base main --review-mode gate > "$repo/incomplete-gate.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "incomplete semantic review should park, got $rc"
  python3 - "$repo/.oms/plan/autopilot-run.json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    diagnostic = json.load(handle)["diagnostic"]
assert diagnostic["reason_code"] == "semantic-review-incomplete", diagnostic
assert diagnostic["semantic_verdict"] == "incomplete", diagnostic
assert "review did not complete" in diagnostic["reason"], diagnostic
assert "oms autopilot run" in diagnostic["reason"], diagnostic
assert "rejected" not in diagnostic["reason"], diagnostic
PY
  run_autopilot "$repo" status > "$repo/incomplete-status.out" 2>&1 ||
    fail "incomplete semantic campaign should remain visible in status"
  grep -Fq 'reason=semantic-review-incomplete' "$repo/incomplete-status.out" ||
    fail "status omitted the incomplete semantic park reason"
  grep -Fq 'semantic=incomplete' "$repo/incomplete-status.out" ||
    fail "status omitted the incomplete semantic verdict"
  grep -Fq 'review did not complete' "$repo/incomplete-status.out" ||
    fail "status omitted why semantic review was incomplete"

  # A run-default worker is not the only author once tasks are assigned.
  write_done_plan "$repo" true
  python3 - "$repo/.oms/plan/tasks.json" <<'PY'
import json,sys
path=sys.argv[1]
with open(path, encoding="utf-8") as handle:
    data=json.load(handle)
task=next(iter(data["tasks"].values()))
task["assignment"]={"provider":"claude"}
task["provider"]="claude"
with open(path,"w",encoding="utf-8") as handle:
    json.dump(data,handle)
PY
  : > "$repo/calls/peer-review"
  : > "$repo/calls/draft-pr"
  rc=0
  OMS_T_GOAL_RESULT=success run_autopilot "$repo" run \
    --planner claude --worker codex --reviewer claude --allowed 'src,tests' \
    --base main --review-mode gate --draft-pr >"$repo/self-review.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "assigned author passed its own review gate"
  grep -Fq 'reviewer-authored-task' "$repo/self-review.out" || fail "assigned authorship was not checked"
  [ ! -s "$repo/calls/peer-review" ] && [ ! -s "$repo/calls/draft-pr" ] ||
    fail "self-review boundary called a reviewer or publisher"

  # Off mode is an operator choice that is disclosed, not hidden: no reviewer
  # call happens and the publication intent carries the skip verbatim.
  write_done_plan "$repo" true
  : > "$repo/calls/draft-pr"
  : > "$repo/calls/peer-review"
  OMS_T_GOAL_RESULT=success run_autopilot "$repo" run \
    --planner claude --worker codex --allowed 'src/,tests/' \
    --base main --draft-pr --review-mode off > "$repo/off.out" 2>&1 ||
    fail "review-mode off should complete the publish path"
  [ ! -s "$repo/calls/peer-review" ] || fail "review-mode off still called the reviewer"
  grep -Fq -- '--review-evidence mode=off outcome=skipped reviewer=none' \
    "$repo/calls/draft-pr" || fail "the skipped review was not disclosed to prepare"

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

  # Branch identity is part of the frozen whole-change snapshot. A reviewer
  # that checks out a sibling branch at the same HEAD/tree must not redirect
  # the subsequent create-only publication to that unreviewed branch name.
  local sibling_repo="$TMP/reviewer-sibling"
  local sibling_head
  make_repo "$sibling_repo"
  mkdir -p "$sibling_repo/calls"
  write_done_plan "$sibling_repo" true
  sibling_head="$(git -C "$sibling_repo" rev-parse HEAD)"
  rc=0
  OMS_T_GOAL_RESULT=success OMS_T_REVIEW_RC=0 \
    OMS_T_REVIEW_SWITCH_BRANCH=oms/autopilot-review-sibling \
    run_autopilot "$sibling_repo" run --worker codex --reviewer claude \
      --base main --review-mode gate --draft-pr \
      > "$sibling_repo/sibling.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "a same-HEAD reviewer branch switch should park, got $rc"
  grep -Fq 'reason=branch-changed-after-review' "$sibling_repo/sibling.out" ||
    fail "reviewer branch drift was not identified"
  [ "$sibling_head" = "$(git -C "$sibling_repo" rev-parse HEAD)" ] ||
    fail "same-HEAD branch fixture unexpectedly changed the reviewed commit"
  [ ! -s "$sibling_repo/calls/draft-pr" ] ||
    fail "a reviewer-selected sibling branch reached Draft PR publication"

  # Whole-branch scope is not a one-time preflight. It is rechecked after the
  # drive, after semantic review, and after Draft PR intent preparation before
  # any publication can happen.
  local drive_scope_repo="$TMP/drive-final-scope"
  make_repo "$drive_scope_repo"
  mkdir -p "$drive_scope_repo/calls"
  write_done_plan "$drive_scope_repo" true
  rc=0
  OMS_T_GOAL_RESULT=success OMS_T_GOAL_COMMIT_OUTSIDE=1 \
    run_autopilot "$drive_scope_repo" run --worker codex --reviewer claude \
      --base main --review-mode gate > "$drive_scope_repo/scope.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "out-of-scope drive result should park, got $rc: $(tail -8 "$drive_scope_repo/scope.out")"
  grep -Fq 'reason=work-branch-outside-scope-after-drive' "$drive_scope_repo/scope.out" ||
    fail "post-drive scope failure was not identified"
  [ ! -s "$drive_scope_repo/calls/peer-review" ] ||
    fail "out-of-scope drive result reached semantic review"

  # Every scope boundary accepts the same glob language. patch-admit and the
  # live change guard already admit src/app.py for src/*.py; the final
  # whole-branch fence must not reinterpret that reviewed pattern as a
  # literal directory prefix and park an otherwise authorized drive.
  local glob_scope_repo="$TMP/drive-glob-scope"
  make_repo "$glob_scope_repo"
  mkdir -p "$glob_scope_repo/calls"
  write_done_plan "$glob_scope_repo" true
  python3 - "$glob_scope_repo/.oms/plan/tasks.json" <<'PY'
import json, sys
path = sys.argv[1]
with open(path, encoding="utf-8") as handle:
    row = json.load(handle)
row["project_contract"]["allowed_envelope"] = ["src/*.py"]
row["tasks"]["t1"]["allowed_paths"] = ["src/*.py"]
with open(path, "w", encoding="utf-8") as handle:
    json.dump(row, handle)
PY
  rc=0
  OMS_T_GOAL_RESULT=success OMS_T_GOAL_COMMIT_GLOB=1 \
    run_autopilot "$glob_scope_repo" run --worker codex --reviewer claude \
      --allowed 'src/*.py' --base main --review-mode gate \
      > "$glob_scope_repo/scope.out" 2>&1 || rc=$?
  [ "$rc" = 0 ] ||
    fail "a path admitted by the reviewed glob should clear the final fence, got $rc: $(tail -8 "$glob_scope_repo/scope.out")"
  [ -s "$glob_scope_repo/calls/peer-review" ] ||
    fail "the glob-authorized drive did not reach semantic review"

  local exhausted_scope_repo="$TMP/exhausted-final-scope"
  make_repo "$exhausted_scope_repo"
  mkdir -p "$exhausted_scope_repo/calls"
  write_done_plan "$exhausted_scope_repo" false
  rc=0
  OMS_T_GOAL_RESULT=exhausted OMS_T_GOAL_COMMIT_OUTSIDE=1 \
    run_autopilot "$exhausted_scope_repo" run --worker codex --reviewer claude \
      --allowed 'src,tests' --base main --review-mode gate \
      > "$exhausted_scope_repo/scope.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "out-of-scope exhausted drive should park, got $rc"
  grep -Fq 'reason=work-branch-outside-scope-after-drive' \
    "$exhausted_scope_repo/scope.out" ||
    fail "exhausted post-drive scope failure was not identified"
  [ ! -s "$exhausted_scope_repo/calls/plan-from-spec" ] ||
    fail "out-of-scope exhausted drive reached the remainder planner"

  local review_scope_repo="$TMP/review-final-scope"
  make_repo "$review_scope_repo"
  mkdir -p "$review_scope_repo/calls"
  write_done_plan "$review_scope_repo" true
  rc=0
  OMS_T_GOAL_RESULT=success OMS_T_REVIEW_COMMIT_OUTSIDE=1 \
    run_autopilot "$review_scope_repo" run --worker codex --reviewer claude \
      --base main --review-mode gate --draft-pr > "$review_scope_repo/scope.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "out-of-scope reviewer result should park, got $rc"
  grep -Fq 'reason=work-branch-outside-scope-after-review' "$review_scope_repo/scope.out" ||
    fail "post-review scope failure was not identified"
  [ ! -s "$review_scope_repo/calls/draft-pr" ] ||
    fail "out-of-scope reviewer result reached Draft PR"

  local prepare_scope_repo="$TMP/prepare-final-scope"
  make_repo "$prepare_scope_repo"
  mkdir -p "$prepare_scope_repo/calls"
  write_done_plan "$prepare_scope_repo" true
  rc=0
  OMS_T_GOAL_RESULT=success OMS_T_REVIEW_RC=0 OMS_T_PREPARE_COMMIT_OUTSIDE=1 \
    run_autopilot "$prepare_scope_repo" run --worker codex --reviewer claude \
      --base main --review-mode gate --draft-pr > "$prepare_scope_repo/scope.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "out-of-scope publish preparation should park, got $rc"
  grep -Fq 'reason=work-branch-outside-scope-before-publication' "$prepare_scope_repo/scope.out" ||
    fail "pre-publication scope failure was not identified"
  if grep -Eq '^publish ' "$prepare_scope_repo/calls/draft-pr"; then
    fail "out-of-scope prepared tree reached Draft PR publish"
  fi

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

  # The wrapper disables repository hooks before its own top-level checkout;
  # a reviewed run cannot execute mutable post-checkout code from the repo.
  local hook_repo="$TMP/top-level-hooks"
  make_repo "$hook_repo"
  mkdir -p "$hook_repo/calls"
  write_done_plan "$hook_repo" true
  git -C "$hook_repo" checkout -q main
  cat > "$hook_repo/.git/hooks/post-checkout" <<'EOF'
#!/usr/bin/env bash
: > "$OMS_T_REPO/calls/post-checkout-fired"
EOF
  chmod +x "$hook_repo/.git/hooks/post-checkout"
  rm -f "$hook_repo/calls/post-checkout-fired"
  OMS_T_GOAL_RESULT=success run_autopilot "$hook_repo" run --worker codex \
    --reviewer claude --base main --review-mode gate > "$hook_repo/hook.out" 2>&1 ||
    fail "hook-suppressed top-level checkout should complete"
  [ ! -e "$hook_repo/calls/post-checkout-fired" ] ||
    fail "autopilot top-level checkout executed a repository hook"

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
  case "$(git -C "$branch_repo" branch --show-current)" in
    oms/autopilot-*) ;;
    *) fail "publish path did not create the provider-neutral work branch" ;;
  esac

  # From the base, an existing recovery suffix proves this contract already has
  # an operator-selected continuation. This must park even when the untouched
  # canonical branch still exists beside the recovery lineage.
  local suffix_repo="$TMP/recovery-suffix-only"
  local canonical_branch recovery_branch
  make_repo "$suffix_repo"
  mkdir -p "$suffix_repo/calls"
  write_done_plan "$suffix_repo" true
  canonical_branch="$(git -C "$suffix_repo" branch --show-current)"
  recovery_branch="$canonical_branch-r2"
  git -C "$suffix_repo" branch "$recovery_branch" "$canonical_branch"
  git -C "$suffix_repo" switch -q "$recovery_branch"
  printf 'recovery lineage\n' >> "$suffix_repo/src/app.txt"
  git -C "$suffix_repo" add src/app.txt
  git -C "$suffix_repo" commit -qm 'fix: preserve recovery lineage'
  git -C "$suffix_repo" switch -q main
  rc=0
  OMS_T_GOAL_RESULT=success run_autopilot "$suffix_repo" run --worker codex \
    --base main > "$suffix_repo/suffix-only.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "an existing recovery suffix should park the base run, got $rc"
  grep -Fq 'reason=autopilot-recovery-branch-exists' "$suffix_repo/suffix-only.out" ||
    fail "canonical/recovery coexistence parked for the wrong reason"
  [ "$(git -C "$suffix_repo" branch --show-current)" = main ] ||
    fail "base run switched branches despite the existing recovery suffix"
  git -C "$suffix_repo" show-ref --verify --quiet "refs/heads/$canonical_branch" ||
    fail "base run lost the existing canonical branch"
  git -C "$suffix_repo" show-ref --verify --quiet "refs/heads/$recovery_branch" ||
    fail "base run lost the existing recovery suffix"
  [ ! -s "$suffix_repo/calls/goal-drive" ] ||
    fail "base run drove work despite the existing recovery suffix"

  # An arbitrary checked-out branch is never silently driven. Recovery names
  # use the strict -rN form, and their complete committed diff must remain
  # inside the reviewed project envelope.
  local foreign_repo="$TMP/foreign-branch"
  local foreign_target
  make_repo "$foreign_repo"
  mkdir -p "$foreign_repo/calls"
  write_done_plan "$foreign_repo" true
  git -C "$foreign_repo" switch -qc feature-x
  rc=0
  OMS_T_GOAL_RESULT=success run_autopilot "$foreign_repo" run --worker codex \
    --base main > "$foreign_repo/foreign.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "a foreign work branch should park, got $rc"
  grep -Fq 'reason=foreign-work-branch' "$foreign_repo/foreign.out" ||
    fail "foreign branch park reason missing"
  [ ! -f "$foreign_repo/calls/goal-drive" ] || fail "a foreign branch reached goal-drive"
  foreign_target="oms/autopilot-$(sha256_file "$foreign_repo/PROJECT.md" | cut -c1-12)"
  rm -f "$foreign_repo/.oms/plan/autopilot-run.json"
  git -C "$foreign_repo" switch -q main
  # A live guarded call (an unreadable entry counts as live) defers the branch
  # switch with 75 and keeps its registry entry.
  mkdir -p "$foreign_repo/.oms/locks/canonical-sync.d/fake-live-call"
  rc=0
  OMS_T_GOAL_RESULT=success run_autopilot "$foreign_repo" run --worker codex \
    --base main > "$foreign_repo/live.out" 2>&1 || rc=$?
  [ "$rc" = 75 ] || fail "a live guarded call should defer the branch switch with 75, got $rc"
  grep -Fq '1 guarded call(s) are live' "$foreign_repo/live.out" ||
    fail "the deferral should name the live count"
  [ -d "$foreign_repo/.oms/locks/canonical-sync.d/fake-live-call" ] ||
    fail "autopilot removed a live registry entry"
  [ "$(git -C "$foreign_repo" symbolic-ref --short HEAD)" = main ] ||
    fail "autopilot switched branches despite a live guarded call"
  rmdir "$foreign_repo/.oms/locks/canonical-sync.d/fake-live-call"
  rm -f "$foreign_repo/.oms/plan/autopilot-run.json"
  # With no live entry, a held canonical sync gate (where a worker registers)
  # still blocks the switch: the check and checkout run under that gate.
  local gate_held="$TMP/gate-held" gate_release="$TMP/gate-release" gate_waited=0
  rm -f "$gate_held" "$gate_release"
  ( bash -c '. "$1/scripts/lib/canonical-sync.sh"
      oms_canonical_sync_prepare "$2" &&
      oms_canonical_sync_gate "$2" bash -c "touch \"\$1\"; while [ ! -e \"\$2\" ]; do sleep 0.2; done" _ "$3" "$4"' \
      _ "$ROOT" "$foreign_repo" "$gate_held" "$gate_release" ) &
  local gate_pid=$!
  while [ ! -e "$gate_held" ] && [ "$gate_waited" -lt "${OMS_TEST_CALL_TIMEOUT:-60}" ]; do
    sleep 0.2
    gate_waited=$((gate_waited + 1))
  done
  [ -e "$gate_held" ] || { : > "$gate_release"; fail "the test could not hold the canonical sync gate"; }
  rc=0
  OMS_T_GOAL_RESULT=success run_autopilot "$foreign_repo" run --worker codex \
    --base main > "$foreign_repo/gate.out" 2>&1 || rc=$?
  : > "$gate_release"
  wait "$gate_pid" || true
  [ "$rc" = 75 ] || fail "a held canonical sync gate should defer the branch switch with 75, got $rc"
  [ "$(git -C "$foreign_repo" symbolic-ref --short HEAD)" = main ] ||
    fail "autopilot switched branches without holding the canonical sync gate"
  rm -f "$foreign_repo/.oms/plan/autopilot-run.json"
  git -C "$foreign_repo" switch -qc "$foreign_target-vendor"
  rc=0
  OMS_T_GOAL_RESULT=success run_autopilot "$foreign_repo" run --worker codex \
    --base main > "$foreign_repo/vendor.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "a non-rN lookalike recovery branch should park, got $rc"
  grep -Fq 'reason=foreign-work-branch' "$foreign_repo/vendor.out" ||
    fail "lookalike recovery branch was rejected for the wrong reason"

  git -C "$foreign_repo" switch -q main
  git -C "$foreign_repo" switch -qc "$foreign_target-r3"
  rm -f "$foreign_repo/.oms/plan/autopilot-run.json"
  printf 'outside reviewed scope\n' > "$foreign_repo/outside.txt"
  git -C "$foreign_repo" add outside.txt
  git -C "$foreign_repo" commit -qm 'test: add foreign recovery history'
  : > "$foreign_repo/calls/goal-drive"
  rc=0
  OMS_T_GOAL_RESULT=success run_autopilot "$foreign_repo" run --worker codex \
    --base main > "$foreign_repo/outside.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "out-of-scope recovery history should park, got $rc"
  grep -Fq 'reason=work-branch-outside-scope' "$foreign_repo/outside.out" ||
    fail "out-of-scope recovery history was rejected for the wrong reason"
  [ ! -s "$foreign_repo/calls/goal-drive" ] ||
    fail "out-of-scope recovery history reached goal-drive"

  # Git's NUL-delimited path uses '/' as the separator on every platform.
  # Build a base tree containing a literal root filename `src\escape` via Git
  # plumbing, then a normal child tree that deletes it. The checked-out tree
  # remains Windows-safe while base..HEAD still exposes the raw pathname.
  local original_main normal_tree invalid_blob invalid_tree invalid_base valid_head
  git -C "$foreign_repo" switch -q main
  original_main="$(git -C "$foreign_repo" rev-parse HEAD)"
  normal_tree="$(git -C "$foreign_repo" rev-parse 'HEAD^{tree}')"
  invalid_blob="$(printf 'literal backslash outside reviewed scope\n' |
    git -C "$foreign_repo" hash-object -w --stdin)"
  invalid_tree="$({
    git -C "$foreign_repo" ls-tree -z "$normal_tree"
    printf '100644 blob %s\tsrc\\escape\0' "$invalid_blob"
  } | git -C "$foreign_repo" mktree -z)"
  invalid_base="$(printf 'test: base with literal backslash path\n' |
    git -C "$foreign_repo" commit-tree "$invalid_tree" -p "$original_main")"
  valid_head="$(printf 'test: delete literal backslash path\n' |
    git -C "$foreign_repo" commit-tree "$normal_tree" -p "$invalid_base")"
  git -C "$foreign_repo" branch "$foreign_target-r4" "$valid_head"
  git -C "$foreign_repo" switch -q "$foreign_target-r4"
  rm -f "$foreign_repo/.oms/plan/autopilot-run.json"
  git -C "$foreign_repo" update-ref refs/heads/main "$invalid_base" "$original_main"
  : > "$foreign_repo/calls/goal-drive"
  rc=0
  OMS_T_GOAL_RESULT=success run_autopilot "$foreign_repo" run --worker codex \
    --base main > "$foreign_repo/backslash.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "literal backslash recovery path should park, got $rc"
  grep -Fq 'reason=work-branch-outside-scope' "$foreign_repo/backslash.out" ||
    fail "literal backslash path bypassed the reviewed scope"
  [ ! -s "$foreign_repo/calls/goal-drive" ] ||
    fail "literal backslash recovery history reached goal-drive"
  # Restore the Windows-safe base before any later checkout; the malformed
  # tree was needed only as the frozen diff endpoint for the assertion above.
  git -C "$foreign_repo" update-ref refs/heads/main "$original_main" "$invalid_base"

  git -C "$foreign_repo" switch -q main
  git -C "$foreign_repo" switch -qc "$foreign_target-r2"
  rm -f "$foreign_repo/.oms/plan/autopilot-run.json"
  OMS_T_GOAL_RESULT=success run_autopilot "$foreign_repo" run --worker codex \
    --base main > "$foreign_repo/recovery.out" 2>&1 ||
    fail "an in-scope -rN recovery branch of the same contract should resume"

  # A second remainder proposal is rejected even when passed directly, and
  # proposal metadata cannot claim r1- while carrying ordinary ids.
  write_done_plan "$repo" true 1
  local second_proposal="$repo/.oms/plan/second-r1.json"
  cat > "$second_proposal" <<'JSON'
{"schema":1,"kind":"agent-plan-proposal","id_prefix":"r1-","allowed_envelope":["src","tests"],"tasks":[{"id":"r1-second","title":"fix: second remainder","allowed":["src"],"verify":"true","depends":[]}]}
JSON
  rc=0
  run_autopilot "$repo" run --proposal "$second_proposal" \
    --expected-proposal-sha256 "$(sha256_file "$second_proposal")" \
    --allowed 'src,tests' --worker codex --base main >/dev/null 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "direct second remainder should be rejected, got $rc"

  # The parent's reviewed digest is required, and bytes that no longer match
  # it are refused before any metadata or apply read.
  rc=0
  run_autopilot "$repo" run --proposal "$second_proposal" --allowed 'src,tests' \
    --worker codex --base main > "$repo/no-digest.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "a proposal without the reviewed digest should be rejected, got $rc"
  grep -Fq 'requires --expected-proposal-sha256' "$repo/no-digest.out" ||
    fail "missing digest was rejected for the wrong reason"
  rc=0
  run_autopilot "$repo" run --proposal "$second_proposal" \
    --expected-proposal-sha256 \
    0000000000000000000000000000000000000000000000000000000000000000 \
    --allowed 'src,tests' --worker codex --base main > "$repo/bad-digest.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "swapped proposal bytes should be rejected, got $rc"
  grep -Fq 'do not match the reviewed' "$repo/bad-digest.out" ||
    fail "digest mismatch was rejected for the wrong reason"

  # The reviewed path itself is part of the approval receipt. Matching target
  # bytes do not authorize a symlink proposal source that can be retargeted
  # between operator review and the read-once snapshot.
  local symlink_repo="$TMP/symlink-proposal"
  local proposal_source proposal_target proposal_link proposal_link_sha
  make_repo "$symlink_repo"
  mkdir -p "$symlink_repo/calls"
  proposal_source="$symlink_repo/proposal-source.json"
  proposal_target="$symlink_repo/.oms/plan/proposal-target.json"
  proposal_link="$symlink_repo/.oms/plan/proposal-link.json"
  write_proposal "$proposal_source"
  mkdir -p "$symlink_repo/.oms/plan"
  mv "$proposal_source" "$proposal_target"
  ln -s "$(basename "$proposal_target")" "$proposal_link"
  proposal_link_sha="$(sha256_file "$proposal_link")"
  rc=0
  run_autopilot "$symlink_repo" run --proposal "$proposal_link" \
    --expected-proposal-sha256 "$proposal_link_sha" --allowed 'src,tests' \
    --worker codex --base main > "$symlink_repo/symlink.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "a symlink proposal source should be a contract error, got $rc"
  [ ! -s "$symlink_repo/calls/plan-from-spec" ] ||
    fail "a symlink proposal reached the atomic apply"
  [ ! -f "$symlink_repo/.oms/plan/tasks.json" ] ||
    fail "a symlink proposal changed plan state"

  # Proposal approval never authorizes an unbounded local read. A regular
  # source above the documented snapshot cap is rejected before atomic apply.
  local oversized_repo="$TMP/oversized-proposal"
  local oversized_proposal oversized_sha
  make_repo "$oversized_repo"
  mkdir -p "$oversized_repo/calls"
  oversized_proposal="$oversized_repo/proposal.json"
  python3 - "$oversized_proposal" <<'PY'
import sys

with open(sys.argv[1], "wb") as handle:
    handle.write(b"x" * (1024 * 1024 + 1))
PY
  oversized_sha="$(sha256_file "$oversized_proposal")"
  rc=0
  run_autopilot "$oversized_repo" run --proposal "$oversized_proposal" \
    --expected-proposal-sha256 "$oversized_sha" --allowed 'src,tests' \
    --worker codex --base main > "$oversized_repo/oversized.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "an oversized proposal should be a contract error, got $rc"
  [ ! -s "$oversized_repo/calls/plan-from-spec" ] ||
    fail "an oversized proposal reached the atomic apply"
  [ ! -f "$oversized_repo/.oms/plan/tasks.json" ] ||
    fail "an oversized proposal changed plan state"

  local bad_prefix="$contract_repo/.oms/plan/bad-prefix.json"
  local bad_spec bad_plan bad_base
  bad_spec="$(sha256_file "$contract_repo/PROJECT.md")"
  bad_plan="$(sha256_file "$contract_repo/.oms/plan/tasks.json")"
  bad_base="$(git -C "$contract_repo" rev-parse HEAD)"
  cat > "$bad_prefix" <<JSON
{"schema":1,"kind":"agent-plan-proposal","spec_sha256":"$bad_spec","plan_sha256":"$bad_plan","base_sha":"$bad_base","id_prefix":"r1-","allowed_envelope":["src","tests"],"acceptance_files":["PROJECT.md"],"tasks":[{"id":"ordinary","title":"fix: bad prefix","allowed":["src"],"verify":"true","depends":[]}]}
JSON
  rc=0
  "$ROOT/scripts/plan-from-spec.sh" --repo "$contract_repo" --apply "$bad_prefix" \
    --expected-proposal-sha256 "$(sha256_file "$bad_prefix")" \
    --allowed 'src,tests' > "$contract_repo/bad-prefix.out" 2>&1 || rc=$?
  [ "$rc" != 0 ] || fail "proposal id_prefix metadata was not enforced"
  grep -Fq 'does not start with required prefix r1-' "$contract_repo/bad-prefix.out" ||
    fail "bad prefix was rejected for the wrong reason"

  rc=0
  OMS_HARNESS_CHILD=1 run_autopilot "$repo" run --worker codex \
    --allowed 'src/' --base main >/dev/null 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "a harness child must not drive the parent workflow"

  # Targeted cancellation owns the complete active provider session. Even a
  # direct child and background descendant that ignore TERM cannot write after
  # the autopilot parent has returned from its bounded TERM→KILL→reap cleanup.
  local signal_repo="$TMP/phase-signal-supervision"
  make_repo "$signal_repo"
  mkdir -p "$signal_repo/calls"
  write_done_plan "$signal_repo" true
  OMS_T_REPO="$signal_repo" OMS_T_CALLS="$signal_repo/calls" \
    OMS_T_GOAL_RESULT=signal-wait \
    OMS_AUTOPILOT_GOAL_DRIVE="$TMP/bin/goal-drive" \
    OMS_AUTOPILOT_PLAN_FROM_SPEC="$TMP/bin/plan-from-spec" \
    OMS_AUTOPILOT_PEER_REVIEW="$TMP/bin/peer-review" \
    OMS_AUTOPILOT_DRAFT_PR="$TMP/bin/draft-pr" \
    "$ROOT/scripts/autopilot.sh" --repo "$signal_repo" run --worker codex \
      --reviewer claude --base main --review-mode gate \
      > "$signal_repo/signal.out" 2>&1 &
  local autopilot_pid=$! signal_tries=0 signal_rc=0
  while [ ! -e "$signal_repo/calls/goal-drive-started" ] && \
      [ "$signal_tries" -lt 200 ]; do
    sleep 0.02
    signal_tries=$((signal_tries + 1))
  done
  [ -e "$signal_repo/calls/goal-drive-started" ] ||
    fail "signal supervision fixture never reached goal-drive"
  kill -TERM "$autopilot_pid"
  wait "$autopilot_pid" || signal_rc=$?
  [ "$signal_rc" = 143 ] ||
    fail "TERM should return 143 after phase cleanup, got $signal_rc"
  # Outwait the fixture's 3s delayed write with a real margin: a thin
  # 200ms window turns a genuine leak into a green on a slow runner.
  sleep 4.5
  [ ! -e "$signal_repo/calls/goal-drive-leaked" ] ||
    fail "goal-drive descendant survived targeted autopilot TERM"

  # A provider shell can also daemonize a setsid child and exit before the
  # next ordinary group check. The supervisor adopts/retains that descendant,
  # drains it before returning, and never lets its delayed write escape.
  local escape_repo="$TMP/phase-escape-supervision"
  make_repo "$escape_repo"
  mkdir -p "$escape_repo/calls"
  write_done_plan "$escape_repo" true
  rc=0
  OMS_T_GOAL_RESULT=escape-and-exit run_autopilot "$escape_repo" run \
    --worker codex --reviewer claude --base main --review-mode gate \
    > "$escape_repo/escape.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "escaped provider child should park, got $rc"
  [ -s "$escape_repo/calls/goal-drive-escaped-pid" ] ||
    fail "setsid escape fixture never started"
  local escaped_pid
  escaped_pid="$(tr -d '\r\n' < "$escape_repo/calls/goal-drive-escaped-pid")"
  if kill -0 "$escaped_pid" 2>/dev/null; then
    fail "setsid provider descendant remained after autopilot returned"
  fi
  sleep 4.5
  [ ! -e "$escape_repo/calls/goal-drive-leaked" ] ||
    fail "setsid provider descendant wrote after autopilot returned"

  # The outer parent passes its exact deterministic ref to goal-drive rather
  # than authorizing whichever same-SHA branch happens to be checked out when
  # the inner process starts.
  local expected_ref_repo="$TMP/expected-ref-binding"
  make_repo "$expected_ref_repo"
  mkdir -p "$expected_ref_repo/calls"
  write_done_plan "$expected_ref_repo" true
  OMS_T_GOAL_RESULT=success OMS_T_GOAL_EXPECT_REF=1 \
    run_autopilot "$expected_ref_repo" run --worker codex --reviewer claude \
      --base main --review-mode gate > "$expected_ref_repo/ref.out" 2>&1 ||
    fail "outer expected-ref binding fixture should complete"
  local observed_ref expected_ref_arg
  observed_ref="$(tr -d '\r\n' < "$expected_ref_repo/calls/goal-drive-observed-ref")"
  expected_ref_arg="$(tr -d '\r\n' < "$expected_ref_repo/calls/goal-drive-expected-ref")"
  [ "$expected_ref_arg" = "$observed_ref" ] ||
    fail "autopilot did not bind goal-drive to its exact work ref"
}


test_auto_collaboration() {
  local repo="$TMP/auto-collab" bin="$TMP/auto-collab-bin" rc proposal sha
  make_repo "$repo"
  mkdir -p "$bin" "$repo/calls"
  write_orchestration_stubs "$TMP/bin"
  cat > "$bin/peer-ask" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$OMS_T_CALLS/council"
if [ "${OMS_T_COUNCIL_SILENT_TIMEOUT:-0}" = 1 ]; then
  log="$(sed -n 's/^plan-from-spec: council log: //p' "$OMS_T_COUNCIL_PARENT_OUTPUT")"
  [ -f "$log" ] && printf '%s\n' "$log" > "$OMS_T_CALLS/prelaunch-log"
  exit 124
fi
[ "${OMS_T_COUNCIL_FAIL:-0}" = 0 ] && [ "${OMS_T_COUNCIL_DROP:-0}" = 0 ] || exit 1
while [ "$#" -gt 0 ]; do
  case "$1" in --artifact-dir) dir="$2"; shift 2 ;; *) shift ;; esac
done
printf '# Peer ask synthesis\n- success: 2/2 providers\nEvidence: council-marker-42\n' > "$dir/synthesis.md"
echo "synthesis: $dir/synthesis.md"
EOF
  cat > "$bin/agent-call" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" > "$OMS_T_CALLS/planner-args"
while [ "$#" -gt 0 ]; do
  case "$1" in --prompt) printf '%s\n' "$2" > "$OMS_T_CALLS/planner-prompt"; shift 2 ;; *) shift ;; esac
done
artifact="$OMS_T_CALLS/planner-artifact.md"
printf '## Output\n\n%s\n' '{"tasks":[{"id":"t1","title":"feat: bounded change","allowed":["src/"],"verify":"true","depends":[],"assignment":{"provider":"codex","model":"gpt-6.1-sol"}}]}' > "$artifact"
echo "artifact: $artifact"
EOF
  chmod +x "$bin"/*
  rc=0
  OMS_PLAN_FROM_SPEC_AGENT_CALL="$bin/agent-call" OMS_CAPABILITY_DIR="$TMP/collab-cap" \
    OMS_T_CALLS="$repo/calls" OMS_AUTOPILOT_PEER_ASK="$bin/peer-ask" \
    "$ROOT/scripts/autopilot.sh" --repo "$repo" --collaboration auto \
      --planner codex --allowed src,tests --base main propose > "$repo/propose.out" 2>&1 || rc=$?
  [ "$rc" = 4 ] || fail "auto collaboration proposal: rc=$rc $(tail -8 "$repo/propose.out")"
  [ ! -e "$repo/.oms/plan/tasks.json" ] || fail "council applied its own proposal"
  grep -Fq -- '--providers codex:model=gpt-6-astra,claude --artifact-dir' "$repo/calls/council" || fail 'council did not pin Codex while preserving Claude routing'
  grep -Fq -- '--repo-context' "$repo/calls/council" || fail 'automatic council missing tracked source context'
  grep -Fq -- '--deliberation' "$repo/calls/council" || fail 'council did not require deliberation'
  if grep -Fq 'Return ONLY one JSON object' "$repo/calls/council"; then
    fail 'decomposition output instructions leaked into council'
  fi
  grep -Fq 'Return ONLY one JSON object' "$repo/calls/planner-prompt" || fail 'planner lost JSON contract'
  grep -Fq -- '--require-complete' "$repo/calls/council" || fail 'council did not require final answers'
  grep -Fq -- '--debate 1' "$repo/calls/council" || fail 'council rebuttal was not bounded'
  grep -Fq 'council-marker-42' "$repo/calls/planner-prompt" || fail 'council evidence never reached planner'
  grep -Fq -- '--collaboration auto' "$repo/propose.out" || fail 'continuation lost collaboration mode'
  python3 - "$repo/.oms/plan/autopilot-run.json" "$ROOT/scripts/lib" <<'PYTEST' || fail 'collaboration receipt contract'
import json, runpy, sys
from pathlib import Path
sys.path.insert(0, sys.argv[2])
rules = runpy.run_path(sys.argv[2] + '/autopilot-receipt.py')
row, _ = rules['load_receipt'](Path(sys.argv[1]))
assert row['contract']['collaboration'] == 'auto'
assert row['contract']['review_mode'] == 'gate'
assert row['routing']['worker']['model'] == 'gpt-6.1-sol'
args = rules['option_args'](row)
assert args[args.index('--collaboration')+1] == 'auto'
# Any GPT-6-or-newer version of a known family passes both validators; 5.x, unknown families and
# malformed or empty IDs do not.
assignment = runpy.run_path(sys.argv[2] + '/task-assignment.py')
for model, fallback, accepted in (('gpt-6.1-terra', '', True), ('gpt-6.2-astra', 'gpt-6.1-terra', True),
                                  ('gpt-5.6-terra', '', False), ('gpt-6.1-sol', 'gpt-5.6-terra', False),
                                  ('gpt-6.1-nova', '', False), ('gpt-6.1.1-sol', '', False),
                                  ('gpt-7-terra', '', True), ('gpt-7.1-astra', 'gpt-6-luna', True),
                                  ('gpt-6.1-sol', 'gpt-7..1-sol', False), ('', '', False)):
    try:
        assignment['validate_collaboration']({'assignment': {'provider': 'codex', 'model': model,
                                                             'fallback_model': fallback}}, 'codex')
        assigned = True
    except ValueError:
        assigned = False
    worker = dict(row['routing']['worker'], model=model, fallback_model=fallback)
    try:
        rules['validate_receipt'](dict(row, routing=dict(row['routing'], worker=worker)))
        received = True
    except rules['ReceiptError']:
        received = False
    assert assigned is accepted and received is accepted, (model, fallback, assigned, received)
PYTEST
  proposal="$(sed -n 's/^plan-from-spec: proposed .* -> //p' "$repo/propose.out")"
  sha="$(sha256_file "$proposal")"
  rc=0
  run_autopilot "$repo" --planner codex --worker-model gpt-6.1-sol \
    --planner-model gpt-6-astra --review-mode gate --allowed src,tests --base main \
    run --proposal "$proposal" --expected-proposal-sha256 "$sha" > "$repo/drop-mode.out" 2>&1 || rc=$?
  [ "$rc" != 0 ] || fail 'resume silently dropped collaboration'
  [ ! -e "$repo/calls/goal-drive" ] || fail 'changed collaboration reached worker'
  grep -Fq 'immutable contract changed (contract)' "$repo/drop-mode.out" || fail 'resume failed for an unrelated reason'
  [ ! -e "$repo/.oms/plan/tasks.json" ] || fail 'changed collaboration mutated plan'

  # A council failure must not quietly become a solo planning call.
  local failed="$TMP/auto-council-failed"
  make_repo "$failed"; mkdir -p "$failed/calls"
  rc=0
  OMS_PLAN_FROM_SPEC_AGENT_CALL="$bin/agent-call" OMS_T_CALLS="$failed/calls" OMS_T_COUNCIL_FAIL=1 \
    OMS_AUTOPILOT_PEER_ASK="$bin/peer-ask" "$ROOT/scripts/plan-from-spec.sh" \
    --repo "$failed" --to codex --collaboration auto > "$failed/failed.out" 2>&1 || rc=$?
  [ "$rc" != 0 ] || fail 'failed council should stop planning'
  grep -Fq 'child exit: 1;' "$failed/failed.out" || fail 'council failure lost child exit'
  [ ! -e "$failed/calls/planner-prompt" ] || fail 'failed council reached planner'
  rc=0
  OMS_PLAN_FROM_SPEC_AGENT_CALL="$bin/agent-call" OMS_T_CALLS="$failed/calls" OMS_T_COUNCIL_DROP=1 \
    OMS_AUTOPILOT_PEER_ASK="$bin/peer-ask" "$ROOT/scripts/plan-from-spec.sh" \
    --repo "$failed" --to codex --collaboration auto > "$failed/dropped.out" 2>&1 || rc=$?
  [ "$rc" != 0 ] || fail 'dropped rebuttal seat should stop planning'
  [ ! -e "$failed/calls/planner-prompt" ] || fail 'incomplete rebuttal reached planner'

  # A silent child timeout must leave a discoverable log before it returns.
  local timed="$TMP/auto-council-silent-timeout" log_path
  make_repo "$timed"; mkdir -p "$timed/calls"
  rc=0
  OMS_PLAN_FROM_SPEC_AGENT_CALL="$bin/agent-call" OMS_T_CALLS="$timed/calls" \
    OMS_T_COUNCIL_SILENT_TIMEOUT=1 OMS_T_COUNCIL_PARENT_OUTPUT="$timed/failed.out" \
    OMS_AUTOPILOT_PEER_ASK="$bin/peer-ask" "$ROOT/scripts/plan-from-spec.sh" \
    --repo "$timed" --to codex --collaboration auto > "$timed/failed.out" 2>&1 || rc=$?
  [ "$rc" != 0 ] || fail 'silent council timeout accepted'
  grep -Fq 'child exit: 124;' "$timed/failed.out" || fail 'council timeout lost child exit'
  [ ! -e "$timed/calls/planner-prompt" ] || fail 'silent timeout reached planner'
  [ ! -e "$timed/.oms/plan/tasks.json" ] || fail 'silent timeout applied tasks'
  [ -s "$timed/calls/prelaunch-log" ] || fail 'council log not discoverable before child exit'
  log_path="$(cat "$timed/calls/prelaunch-log")"
  [ -f "$log_path" ] || fail 'council timeout log was not retained'
  grep -Fq "council log: $log_path" "$timed/failed.out" || fail 'council failure lost log path'

  # Planner-only effort must not silently change the independent council seat.
  local effort_repo planner effort
  for planner in codex claude; do
    effort=high; [ "$planner" != claude ] || effort=max
    effort_repo="$TMP/auto-council-effort-$planner"
    make_repo "$effort_repo"; mkdir -p "$effort_repo/calls"
    OMS_PLAN_FROM_SPEC_AGENT_CALL="$bin/agent-call" OMS_T_CALLS="$effort_repo/calls" \
      OMS_AUTOPILOT_PEER_ASK="$bin/peer-ask" "$ROOT/scripts/plan-from-spec.sh" \
      --repo "$effort_repo" --to "$planner" --collaboration auto --reasoning-effort "$effort" \
      > "$effort_repo/propose.out" 2>&1 || fail 'planner effort rejected by council'
    grep -Fq -- "--reasoning-effort $effort" "$effort_repo/calls/planner-args" || fail 'planner effort lost'
    if grep -Fq -- '--reasoning-effort' "$effort_repo/calls/council"; then
      fail 'planner effort leaked into council'
    fi
    grep -Fq "council reasoning effort: auto (provider defaults); decomposition reasoning effort: $effort" \
      "$effort_repo/propose.out" || fail 'independent council effort not disclosed'
  done

  # Revalidate reviewed assignments at apply, not just generated prose.
  local rejected="$TMP/auto-assignment-rejected" assignment
  make_repo "$rejected"
  for assignment in '{"provider":"codex","model":"gpt-5.5"}' \
    '{"provider":"codex","workload":"routine"}' \
    '{"provider":"codex","model":"gpt-6.1-sol","fallback_model":"gpt-5.6-sol"}' \
    '{"provider":"codex","model":"gpt-5.6-terra"}' \
    '{"provider":"claude","model":"claude-opus-5-5"}'; do
    write_proposal "$rejected/proposal.json"
    python3 - "$rejected/proposal.json" "$assignment" <<'PYTEST'
import json,sys
p=sys.argv[1]; d=json.load(open(p)); d['tasks'][0]['assignment']=json.loads(sys.argv[2])
with open(p,'w') as f: json.dump(d,f)
PYTEST
    sha="$(sha256_file "$rejected/proposal.json")"
    if "$ROOT/scripts/plan-from-spec.sh" --repo "$rejected" --collaboration auto \
      --worker-provider codex --apply "$rejected/proposal.json" \
      --expected-proposal-sha256 "$sha" --allowed src,tests > "$rejected/reject.out" 2>&1; then
      fail "unsafe collaboration assignment applied: $assignment"
    fi
    [ ! -e "$rejected/.oms/plan/tasks.json" ] || fail 'rejected assignment mutated plan'
  done

  # Claude can implement a whole campaign, with Codex as the review gate.
  local claude_repo="$TMP/auto-claude-worker"
  make_repo "$claude_repo"; mkdir -p "$claude_repo/calls"
  write_done_plan "$claude_repo" true 0 0 claude
  OMS_T_GOAL_RESULT=success run_autopilot "$claude_repo" --collaboration auto \
    --worker claude --base main run > "$claude_repo/run.out" 2>&1 ||
    fail "Claude worker campaign failed: $(tail -8 "$claude_repo/run.out")"
  grep -Fq -- '--to claude' "$claude_repo/calls/goal-drive" || fail 'Claude worker not dispatched'
  grep -Fq -- '--providers codex' "$claude_repo/calls/peer-review" || fail 'Claude worker lacks Codex reviewer'
  grep -Fq -- '--model gpt-6-astra' "$claude_repo/calls/peer-review" || fail 'Codex reviewer unpinned'

  local replan="$TMP/auto-replan"
  make_repo "$replan"; mkdir -p "$replan/calls"
  write_done_plan "$replan" false
  rc=0
  OMS_T_GOAL_RESULT=exhausted run_autopilot "$replan" --collaboration auto \
    --allowed src,tests --base main run > "$replan/replan.out" 2>&1 || rc=$?
  [ "$rc" = 4 ] || fail "auto remainder did not return to parent review: $rc"
  grep -Fq -- '--collaboration auto' "$replan/calls/plan-from-spec" || fail 'replan lost council trigger'
  grep -Fq -- '--id-prefix r1-' "$replan/calls/plan-from-spec" || fail 'replan lost bounded tranche'
}

test_autopilot_acceptance_reuse() {
  local policy repo expected
  for policy in fresh same-run; do
    repo="$TMP/reuse-autopilot-$policy"
    make_repo "$repo"
    if [ "$policy" = same-run ]; then
      printf '%s\n' '- Acceptance reuse: same-run' >> "$repo/PROJECT.md"
      git -C "$repo" add PROJECT.md
      git -C "$repo" commit -qm 'test: declare pure acceptance'
      git -C "$repo" branch -f main HEAD
      git -C "$repo" branch -m "oms/autopilot-$(sha256_file "$repo/PROJECT.md" | cut -c1-12)"
    fi
    mkdir -p "$repo/calls"
    write_done_plan "$repo" 'printf x >> .oms/count'
    # Only the semantic provider is a fixture; driver, acceptance supervisor,
    # receipt bindings and both parent acceptance boundaries are real.
    cat > "$TMP/reuse-review" <<'EOF'
#!/usr/bin/env bash
set -eu
while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) repo="$2"; shift 2 ;;
    --verify) verify="$2"; shift 2 ;;
    *) shift ;;
  esac
done
cd "$repo"
bash -c "$verify"
EOF
    chmod +x "$TMP/reuse-review"
    OMS_AUTOPILOT_PEER_REVIEW="$TMP/reuse-review" \
      "$ROOT/scripts/autopilot.sh" --repo "$repo" --base main run \
      > "$repo/run.out" 2>&1 || fail "real reuse orchestration: $(tail -12 "$repo/run.out")"
    expected=xxxx
    if [ "$policy" = same-run ]; then
      expected=xxx
      grep -Fq 'reused pass run=' "$repo/run.out" || fail 'autopilot did not reuse intermediate pass'
    fi
    [ "$(cat "$repo/.oms/count")" = "$expected" ] ||
      fail "autopilot $policy skipped a fresh boundary or repeated the reusable check"
  done
}

test_autopilot_runs_from_verified_scratch_and_allows_ordinary_propose() {
  local main="$TMP/scratch-run-main" managed="$TMP/scratch-run-managed"
  local scratch dead_scratch dead_owner normal discovered refused failed none rc=0 parent_env
  local original_receipt abandoned_archive abandon_record interrupted_branch candidate
  local target frozen_base status_repo receipt_before matrix owner unrelated bound stage caller action
  local expected status_line
  make_repo "$main"
  # The main checkout holds the base branch, so the scratch stays detached at it.
  git -C "$main" switch -q main
  mkdir -p "$main/calls" "$TMP/bin" "$managed"
  write_orchestration_stubs "$TMP/bin"
  main="$(cd "$main" && pwd -P)"
  frozen_base="$(git -C "$main" rev-parse HEAD)"
  target="oms/autopilot-$(sha256_file "$main/PROJECT.md" | cut -c1-12)"
  parent_env="env -u OMS_HARNESS_CHILD -u OMS_HARNESS_DELEGATE_DEPTH OMS_DELEGATE_WORKTREE_ROOT=$managed"

  # Admission matrix: a receipt bound to one execution checkout admits only
  # status and abandon from any other checkout, and nothing from a bound
  # scratch whose owner has exited; every refusal leaves Git and .oms intact.
  sleep 300 &
  dead_owner=$!
  dead_scratch="$($parent_env "$ROOT/scripts/scratch-worktree.sh" add --repo "$main" --owner-pid "$dead_owner")" ||
    fail "dead-owner scratch fixture creation failed"
  scratch="$($parent_env "$ROOT/scripts/scratch-worktree.sh" add --repo "$main" --owner-pid $$)" ||
    fail "scratch worktree fixture creation failed"
  matrix="$TMP/scratch-run-matrix"
  mkdir -p "$dead_scratch/calls" "$scratch/calls" "$matrix"
  rc=0
  (
    unset OMS_HARNESS_CHILD OMS_HARNESS_DELEGATE_DEPTH
    OMS_DELEGATE_WORKTREE_ROOT="$managed" OMS_T_PLAN_RC=3 \
      run_autopilot "$dead_scratch" propose --planner claude --allowed 'src,tests' --base main
  ) > "$matrix/bind.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "the soon-dead scratch should leave an interrupted receipt, got $rc: $(tail -5 "$matrix/bind.out")"
  kill "$dead_owner"
  wait "$dead_owner" 2>/dev/null || true
  owner="$(python3 "$ROOT/scripts/lib/autopilot-receipt.py" owner-id "$main/.oms/plan/autopilot-run.json")" ||
    fail "the interrupted scratch receipt has no owner"
  owner="${owner//$'\r'/}"
  # The dead session left a markerless claimed lease that reenter would recover.
  python3 - "$main/.oms/plan/tasks.json" "$main/PROJECT.md" "$owner" <<'PY' ||
import datetime, hashlib, json, sys
now = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")
spec = hashlib.sha256(open(sys.argv[2], "rb").read()).hexdigest()
json.dump({
    "schema": 3, "goal": "finish", "accept": "true",
    "project_contract": {
        "schema": 1, "spec_sha256": spec,
        "allowed_envelope": ["src", "tests"],
        "acceptance_files": [], "acceptance_manifest": [],
    },
    "tasks": {"t9": {
        "id": "t9", "title": "feat: t9", "state": "claimed",
        "depends": [], "allowed_paths": ["src/"], "forbidden_paths": [],
        "verify": "true", "role": "", "provider": "codex", "ttl": "",
        "artifact": "", "patch": "", "reason": "", "claimed_at": now,
        "executor_id": "", "executor_soul_sha256": "", "lease_epoch": 1,
        "lease_id": "lease-dead-claim", "review_lease_id": "",
        "repair_count": 0, "repair_artifact": "", "created": now, "updated": now,
        "autopilot_owner_id": sys.argv[3],
    }},
}, open(sys.argv[1], "w", encoding="utf-8"))
PY
    fail "could not plant the dead scratch's lease"
  matrix_state() {
    checkout_state "$main" "$dead_scratch" "$scratch" "$unrelated"
  }
  matrix_refused() {  # CHECKOUT EXPECTED-TEXT ACTION ARGS...
    local checkout="$1" expected="$2" action="$3" before matrix_rc=0
    shift 3
    before="$(matrix_state)"
    (
      unset OMS_HARNESS_CHILD OMS_HARNESS_DELEGATE_DEPTH
      OMS_DELEGATE_WORKTREE_ROOT="$managed" OMS_T_STATE_REPO="$main" \
        OMS_T_GOAL_RESULT=success OMS_T_GOAL_COMMIT=1 OMS_T_REVIEW_RC=0 \
        run_autopilot "$checkout" "$action" "$@"
    ) > "$matrix/$action.out" 2>&1 || matrix_rc=$?
    [ "$matrix_rc" = 2 ] ||
      fail "autopilot $action from $checkout must refuse, got $matrix_rc: $(tail -5 "$matrix/$action.out")"
    grep -Fq -- "$expected" "$matrix/$action.out" ||
      fail "autopilot $action from $checkout was not refused by admission: $(tail -5 "$matrix/$action.out")"
    [ "$before" = "$(matrix_state)" ] ||
      fail "refused autopilot $action from $checkout changed HEAD, refs or .oms state"
  }
  matrix_bind() {  # BOUND-CHECKOUT STAGE: restore the plan with the receipt rebound
    rm -rf "$main/.oms/plan"
    cp -R "$matrix/plan" "$main/.oms/plan"
    python3 - "$main/.oms/plan/autopilot-run.json" "$1" "$2" <<'PY' ||
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
row["repo"], row["stage"] = sys.argv[2], sys.argv[3]
open(sys.argv[1], "w", encoding="utf-8").write(json.dumps(row, sort_keys=True) + "\n")
PY
      fail "could not rebind the matrix receipt"
  }
  # Caller x bound owner x stage x action. status reads from every checkout of
  # the state root and abandon retires from any of them, whatever the owner;
  # run, reenter, propose and shadow admit only the bound checkout of a live
  # owner, except that a done receipt binds nothing. An admitted run, reenter or propose stops at the
  # --collaboration probe right after admission, before any effect.
  unrelated="$TMP/scratch-run-unrelated"
  make_repo "$unrelated"
  mkdir -p "$unrelated/calls"
  cp -R "$main/.oms/plan" "$matrix/plan"
  for bound in "$dead_scratch" "$scratch"; do
    for stage in proposing 'done'; do
      matrix_bind "$bound" "$stage"
      status_line=""
      for caller in "$main" "$scratch" "$dead_scratch" "$unrelated"; do
        for action in status abandon run reenter propose shadow; do
          case "$caller:$action" in
            "$unrelated":status|"$unrelated":abandon|"$unrelated":shadow) expected="" ;;
            "$unrelated":*) expected='--collaboration must be off or auto' ;;
            *:status|*:abandon) expected="" ;;
            "$dead_scratch":*) expected='has exited' ;;
            "$bound":shadow) expected="" ;;
            "$bound":*) expected='--collaboration must be off or auto' ;;
            *) expected='is bound to execution checkout' ;;
          esac
          # A done receipt binds nothing: a foreign checkout may start the next
          # campaign over it (shadow only observes, so its cell is skipped).
          if [ "$stage" = 'done' ] && [ "$expected" = 'is bound to execution checkout' ]; then
            case "$action" in
              shadow) continue ;;
              *) expected='--collaboration must be off or auto' ;;
            esac
          fi
          case "$action" in
            run|reenter|propose) set -- --collaboration probe ;;
            shadow) set -- --session matrix ;;
            abandon) set -- --reason 'matrix abandon' ;;
            *) set -- ;;
          esac
          if [ -n "$expected" ]; then
            matrix_refused "$caller" "$expected" "$action" "$@"
            continue
          fi
          receipt_before="$(matrix_state)"
          rc=0
          (
            unset OMS_HARNESS_CHILD OMS_HARNESS_DELEGATE_DEPTH
            OMS_DELEGATE_WORKTREE_ROOT="$managed" run_autopilot "$caller" "$action" "$@"
          ) > "$matrix/$action.out" 2>&1 || rc=$?
          case "$caller:$action:$stage" in
            *:status:*|"$unrelated":*|*:abandon:done)
              [ "$receipt_before" = "$(matrix_state)" ] ||
                fail "autopilot $action from $caller changed HEAD, refs or .oms state" ;;
          esac
          case "$caller:$action:$stage" in
            "$unrelated":status:*|"$unrelated":shadow:*)
              [ "$rc" = 0 ] && ! grep -Fq 'outer run:' "$matrix/$action.out" &&
                ! grep -Fq 'would-' "$matrix/$action.out" ||
                fail "$action from an unrelated repository read the shared receipt: $(tail -5 "$matrix/$action.out")" ;;
            "$unrelated":abandon:*)
              [ "$rc" = 2 ] && grep -Fq 'no live autopilot receipt' "$matrix/$action.out" ||
                fail "abandon from an unrelated repository reached the shared receipt, got $rc: $(tail -5 "$matrix/$action.out")" ;;
            *:status:*)
              # Every checkout of the state root diagnoses the receipt alike.
              [ "$rc" = 0 ] || fail "autopilot status from $caller failed: $(tail -5 "$matrix/$action.out")"
              [ -n "$status_line" ] || status_line="$(grep -F 'outer run:' "$matrix/$action.out" || true)"
              [ "$status_line" = "$(grep -F 'outer run:' "$matrix/$action.out" || true)" ] ||
                fail "autopilot status from $caller diagnosed the $stage receipt unlike the canonical checkout: $(cat "$matrix/$action.out")"
              [ "$stage" = 'done' ] || [ "$status_line" = 'outer run: stage=proposing receipt='"$(sha256_file "$main/.oms/plan/autopilot-run.json")" ] ||
                fail "autopilot status from $caller did not validate the proposing receipt: $(cat "$matrix/$action.out")" ;;
            *:abandon:done)
              [ "$rc" = 2 ] && grep -Fq 'archived, not abandoned' "$matrix/$action.out" ||
                fail "abandon of a done receipt from $caller must refuse, got $rc: $(tail -5 "$matrix/$action.out")" ;;
            *:abandon:*)
              [ "$rc" = 0 ] && [ ! -e "$main/.oms/plan/autopilot-run.json" ] ||
                fail "abandon from $caller did not retire the shared receipt, got $rc: $(tail -5 "$matrix/$action.out")"
              matrix_bind "$bound" "$stage" ;;
            *:shadow:*)
              [ "$rc" = 0 ] && grep -Fq 'would-' "$matrix/$action.out" ||
                fail "shadow from the live bound checkout was not admitted, got $rc: $(tail -5 "$matrix/$action.out")"
              matrix_bind "$bound" "$stage" ;;
          esac
        done
      done
    done
  done
  # A finished scratch campaign whose owner exited does not lock the canonical
  # checkout out: its propose archives the done receipt and binds a new one.
  matrix_bind "$dead_scratch" 'done'
  rm -f "$main/.oms/plan/tasks.json"
  receipt_before="$(sha256_file "$main/.oms/plan/autopilot-run.json")"
  rc=0
  (
    unset OMS_HARNESS_CHILD OMS_HARNESS_DELEGATE_DEPTH
    OMS_DELEGATE_WORKTREE_ROOT="$managed" OMS_T_STATE_REPO="$main" \
      run_autopilot "$main" propose --planner claude --allowed 'src,tests' --base main
  ) > "$matrix/done-propose.out" 2>&1 || rc=$?
  [ "$rc" = 4 ] && [ -f "$main/.oms/plan/autopilot-run.$receipt_before.json" ] ||
    fail "canonical propose over a dead scratch's done receipt must archive it, got $rc: $(tail -5 "$matrix/done-propose.out")"
  # The helper itself refuses a state-root caller on a mutating path, for the
  # race where the shell admission saw no receipt and a scratch bound one after.
  matrix_bind "$scratch" proposing
  receipt_before="$(matrix_state)"
  rc=0
  OMS_DELEGATE_WORKTREE_ROOT="$managed" env -u OMS_HARNESS_CHILD -u OMS_HARNESS_DELEGATE_DEPTH \
    python3 "$ROOT/scripts/lib/autopilot-receipt.py" reenter "$main/.oms/plan/autopilot-run.json" \
    --repo "$main" --state-repo "$main" --updated 2026-01-01T00:00:00Z \
    > "$matrix/helper-reenter.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "helper reenter by the state-root caller must refuse, got $rc: $(tail -3 "$matrix/helper-reenter.out")"
  grep -Fq 'digest gate: outer receipt belongs to a different repository' "$matrix/helper-reenter.out" ||
    fail "helper reenter was not refused by the bound-checkout check: $(tail -3 "$matrix/helper-reenter.out")"
  grep -Fq '"disposition":"resumed"' "$main/.oms/plan/autopilot-reentries.jsonl" 2>/dev/null &&
    fail "helper reenter by the state-root caller recorded a resume"
  # A completed legacy receipt (written before owner_id existed) is archived by
  # the next run, which generates its own owner.
  local legacy="$TMP/scratch-run-legacy"
  make_repo "$legacy"
  mkdir -p "$legacy/calls"
  write_done_plan "$legacy" true
  OMS_T_GOAL_RESULT=success run_autopilot "$legacy" run --worker codex --reviewer claude \
    --base main --review-mode gate > "$legacy/first.out" 2>&1 ||
    fail "legacy receipt fixture run failed: $(tail -5 "$legacy/first.out")"
  python3 - "$legacy/.oms/plan/autopilot-run.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["stage"] == "done", row["stage"]
del row["owner_id"]
open(sys.argv[1], "w", encoding="utf-8").write(json.dumps(row, sort_keys=True) + "\n")
PY
  write_done_plan "$legacy" true 0 1
  # From the base, the run switches branches, which its preflight guards.
  git -C "$legacy" switch -q main
  receipt_before="$(sha256_file "$legacy/.oms/plan/autopilot-run.json")"
  OMS_T_GOAL_RESULT=success run_autopilot "$legacy" run --worker codex --reviewer claude \
    --base main --review-mode gate > "$legacy/second.out" 2>&1 ||
    fail "a run over a completed legacy receipt must be admitted: $(tail -5 "$legacy/second.out")"
  [ -f "$legacy/.oms/plan/autopilot-run.$receipt_before.json" ] ||
    fail "a run over a completed legacy receipt did not archive it"
  matrix_bind "$dead_scratch" proposing
  # A scratch cannot prove its binding from an unreadable receipt.
  cp "$main/.oms/plan/autopilot-run.json" "$matrix/receipt"
  for receipt_before in '{' '' '{"repo": "", "stage": "driving"}'; do
    printf '%s' "$receipt_before" > "$main/.oms/plan/autopilot-run.json"
    matrix_refused "$scratch" 'cannot read the outer receipt binding' run --worker codex --reviewer claude \
      --base main --review-mode gate
  done
  cp "$matrix/receipt" "$main/.oms/plan/autopilot-run.json"

  # Abandon is the cross-root exit and retires the batch the dead scratch bound.
  python3 - "$main/.oms/plan/progress.jsonl" "$dead_scratch" "$owner" <<'PY' ||
import json, sys
row = {"schema": 1, "kind": "goal-batch", "phase": "prepared", "batch_id": "batch-scratch",
       "contract_id": sys.argv[3], "worktree": sys.argv[2], "tasks": []}
with open(sys.argv[1], "a", encoding="utf-8") as handle:
    handle.write(json.dumps(row) + "\n")
PY
    fail "could not seed the scratch batch"
  (
    unset OMS_HARNESS_CHILD OMS_HARNESS_DELEGATE_DEPTH
    OMS_DELEGATE_WORKTREE_ROOT="$managed" \
      run_autopilot "$main" abandon --reason 'scratch run fixture'
  ) > "$matrix/abandon.out" 2>&1 || fail "autopilot abandon from the canonical checkout failed: $(tail -5 "$matrix/abandon.out")"
  grep -Fq '"batch_id": "batch-scratch"' "$main/.oms/plan/progress.jsonl" &&
    grep -F '"batch_id": "batch-scratch"' "$main/.oms/plan/progress.jsonl" | grep -Fq '"phase": "complete"' ||
    fail "canonical abandon did not retire the scratch's prepared batch"
  [ ! -e "$main/.oms/plan/autopilot-run.json" ] ||
    fail "scratch abandon left the shared receipt live"
  find "$main/.oms/plan" -maxdepth 1 -name 'autopilot-run.*.abandoned.json' -print | grep -q . ||
    fail "scratch abandon did not record its exit in the main plan"
  [ ! -e "$main/.oms/plan/autopilot-reentries.jsonl" ] && [ ! -e "$main/.oms/plan/autopilot-shadow.jsonl" ] ||
    fail "a refused reenter or shadow wrote its ledger"
  rm -f "$main/.oms/plan/tasks.json"

  mkdir -p "$TMP/scratch-run-bin"
  # The real plan-from-spec writes the proposal; only the provider is stubbed.
  cat > "$TMP/scratch-run-bin/agent-call" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" > "$OMS_T_CALLS/planner-args"
artifact="$OMS_T_CALLS/planner-artifact.md"
printf '## Output\n\n%s\n' '{"tasks":[{"id":"t1","title":"feat: bounded change","allowed":["src/"],"verify":"true","depends":[]}]}' > "$artifact"
echo "artifact: $artifact"
EOF
  chmod +x "$TMP/scratch-run-bin/agent-call"
  git -C "$scratch" -c user.name=OMS -c user.email=oms@example.invalid \
    commit -q --allow-empty -m 'detached work off the base'
  rc=0
  (
    unset OMS_HARNESS_CHILD OMS_HARNESS_DELEGATE_DEPTH
    OMS_DELEGATE_WORKTREE_ROOT="$managed" OMS_T_STATE_REPO="$main" \
      run_autopilot "$scratch" propose --planner claude --allowed 'src,tests' --base main
  ) > "$scratch/detached.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "a scratch detached off the frozen base must refuse, got $rc: $(tail -5 "$scratch/detached.out")"
  grep -Fq 'HEAD detached at the frozen base' "$scratch/detached.out" ||
    fail "detached-off-base refusal did not explain the accepted HEAD"
  [ ! -e "$main/.oms/plan/autopilot-run.json" ] && [ ! -f "$scratch/calls/plan-from-spec" ] ||
    fail "a scratch detached off the frozen base bound a receipt or reached the planner"
  git -C "$scratch" checkout -q --detach "$frozen_base"
  rc=0
  (
    unset OMS_HARNESS_CHILD OMS_HARNESS_DELEGATE_DEPTH
    OMS_DELEGATE_WORKTREE_ROOT="$managed" OMS_T_CALLS="$scratch/calls" \
      OMS_PLAN_FROM_SPEC_AGENT_CALL="$TMP/scratch-run-bin/agent-call" \
      "$ROOT/scripts/autopilot.sh" --repo "$scratch" propose --planner claude \
        --allowed 'src,tests' --base main
  ) > "$scratch/propose.out" 2>&1 || rc=$?
  [ "$rc" = 4 ] || fail "autopilot propose from verified scratch should reach review, got $rc: $(tail -5 "$scratch/propose.out")"
  [ -f "$scratch/calls/planner-args" ] ||
    fail "autopilot propose from verified scratch did not reach the planner"
  if find "$scratch/.oms/plan" -name 'proposal-*.json' -print 2>/dev/null | grep -q .; then
    fail "plan-from-spec wrote the scratch proposal in the scratch"
  fi
  [ ! -e "$scratch/.oms/plan/autopilot-run.json" ] ||
    fail "autopilot propose from verified scratch wrote its receipt in the scratch"
  python3 - "$main/.oms/plan/autopilot-run.json" "$scratch" "$main/.oms/plan" <<'PY' ||
import json, os, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["stage"] == "proposal-review", row["stage"]
assert os.path.realpath(row["repo"]) == os.path.realpath(sys.argv[2]), row["repo"]
proposal = os.path.realpath(row["proposal"]["path"])
assert os.path.dirname(proposal) == os.path.realpath(sys.argv[3]), proposal
PY
    fail "scratch propose receipt is not in the main plan or does not bind the scratch checkout"
  [ -z "$(git -C "$scratch" symbolic-ref --quiet HEAD 2>/dev/null)" ] &&
    [ "$(git -C "$scratch" rev-parse HEAD)" = "$frozen_base" ] ||
    fail "scratch propose moved the scratch off its detached base"
  for status_repo in "$scratch" "$main"; do
    (
      unset OMS_HARNESS_CHILD OMS_HARNESS_DELEGATE_DEPTH
      OMS_DELEGATE_WORKTREE_ROOT="$managed" run_autopilot "$status_repo" status
    ) > "$status_repo/status.out" 2>&1 || fail "autopilot status failed in $status_repo"
    grep -Fq 'outer run: stage=proposal-review' "$status_repo/status.out" ||
      fail "autopilot status in $status_repo did not read the shared receipt: $(cat "$status_repo/status.out")"
  done
  write_done_plan "$main" true
  rc=0
  (
    unset OMS_HARNESS_CHILD OMS_HARNESS_DELEGATE_DEPTH
    OMS_DELEGATE_WORKTREE_ROOT="$managed" OMS_T_STATE_REPO="$main" \
      OMS_T_GOAL_RESULT=success OMS_T_GOAL_COMMIT=1 OMS_T_REVIEW_RC=0 \
      run_autopilot "$scratch" run --worker codex --reviewer claude \
        --base main --review-mode gate
  ) > "$scratch/run.out" 2>&1 || rc=$?
  [ "$rc" = 0 ] || fail "autopilot run from verified scratch should finish, got $rc: $(tail -8 "$scratch/run.out")"
  [ "$(git -C "$scratch" branch --show-current)" = "$target" ] ||
    fail "scratch run did not drive the deterministic work branch in the scratch"
  [ "$(git -C "$scratch" log -1 --format=%s)" = 'feat: implement planned work' ] ||
    fail "scratch run did not commit the drive on the scratch work branch"
  [ "$(git -C "$main" branch --show-current)" = main ] &&
    [ "$(git -C "$main" rev-parse HEAD)" = "$frozen_base" ] ||
    fail "scratch run moved the main checkout"
  [ -f "$main/.oms/plan/progress.jsonl" ] && [ ! -e "$scratch/.oms/plan/progress.jsonl" ] ||
    fail "scratch run progress did not stay in the main plan"
  python3 - "$main/.oms/plan/autopilot-run.json" "$scratch" "$target" <<'PY' ||
import json, os, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["stage"] == "done", row["stage"]
assert os.path.realpath(row["repo"]) == os.path.realpath(sys.argv[2]), row["repo"]
assert row["branch"] == sys.argv[3], row["branch"]
PY
    fail "scratch run did not finish its receipt in the main plan"
  [ ! -e "$scratch/.oms/plan/autopilot-run.json" ] ||
    fail "scratch run wrote a receipt in the scratch"

  normal="$TMP/scratch-refusal-normal"
  make_repo "$normal"
  mkdir -p "$normal/calls"
  rc=0
  (
    unset OMS_HARNESS_CHILD OMS_HARNESS_DELEGATE_DEPTH
    OMS_DELEGATE_WORKTREE_ROOT="$managed" \
      run_autopilot "$normal" propose --planner claude --allowed 'src,tests' --base main
  ) > "$normal/propose.out" 2>&1 || rc=$?
  [ "$rc" = 4 ] || fail "ordinary checkout should propose normally, got $rc: $(tail -5 "$normal/propose.out")"
  [ -f "$normal/calls/plan-from-spec" ] ||
    fail "ordinary checkout did not reach the stub planner"
  [ -f "$normal/.oms/plan/autopilot-run.json" ] ||
    fail "ordinary checkout did not write the outer receipt"
  ! grep -Fq -- '--discover' "$normal/calls/plan-from-spec" ||
    fail "plain propose passed --discover to the planner"

  discovered="$TMP/scratch-refusal-discover"
  make_repo "$discovered"
  mkdir -p "$discovered/calls"
  rc=0
  run_autopilot "$discovered" propose --discover --planner claude \
    --allowed 'src,tests' --base main > "$discovered/propose.out" 2>&1 || rc=$?
  [ "$rc" = 4 ] || fail "discover propose should reach proposal review, got $rc: $(tail -5 "$discovered/propose.out")"
  [ "$(awk '{ for (i = 1; i <= NF; i++) if ($i == "--discover") count++ } END { print count + 0 }' \
    "$discovered/calls/plan-from-spec")" = 1 ] ||
    fail "discover propose must pass --discover exactly once"

  refused="$TMP/scratch-refusal-discover-run"
  make_repo "$refused"
  rc=0
  run_autopilot "$refused" run --discover --base main > "$refused/run.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "run --discover should be refused, got $rc"
  grep -Fq -- '--discover is only valid with propose' "$refused/run.out" ||
    fail "run --discover refusal did not explain its action restriction"

  failed="$TMP/scratch-refusal-discover-failure"
  make_repo "$failed"
  mkdir -p "$failed/calls"
  rc=0
  OMS_T_PLAN_RC=2 run_autopilot "$failed" propose --discover --planner claude \
    --allowed 'src,tests' --base main > "$failed/propose.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "collector refusal should fail propose with status 2, got $rc"
  grep -Fq 'plan-from-spec: work evidence budget exceeded' "$failed/propose.out" ||
    fail "collector stderr was not visible in the failed propose"
  grep -Fq 'plan-from-spec: proposed' "$failed/propose.out" &&
    fail "collector refusal unexpectedly generated a proposal"
  if python3 - "$failed/.oms/plan/autopilot-run.json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    raise SystemExit(0 if json.load(handle).get("stage") == "proposal-review" else 1)
PY
  then
    fail "collector refusal wrote a proposal-review receipt"
  fi
  if find "$failed/.oms/plan" -maxdepth 1 -name 'autopilot-run.*.abandoned.json' -print | grep -q .; then
    fail "collector refusal unexpectedly wrote an abandon record"
  fi

  none="$TMP/scratch-refusal-discover-empty"
  make_repo "$none"
  mkdir -p "$none/calls"
  rc=0
  OMS_T_PLAN_RESULT=empty-discovery run_autopilot "$none" propose --discover \
    --planner claude --allowed 'src,tests' --base main > "$none/propose.out" 2>&1 || rc=$?
  [ "$rc" = 0 ] || fail "empty discovery should succeed, got $rc: $(tail -5 "$none/propose.out")"
  grep -Fq 'autopilot: discovery found no eligible work' "$none/propose.out" ||
    fail "empty discovery did not explain that no eligible work was found"
  [ ! -e "$none/.oms/plan/autopilot-run.json" ] ||
    fail "empty discovery left a live outer receipt"
  set -- "$none"/.oms/plan/autopilot-run.*.abandoned.json
  [ -f "$1" ] || fail "empty discovery did not write a typed abandon record"
  abandon_record="$1"
  python3 - "$abandon_record" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    row = json.load(handle)
raise SystemExit(0 if row.get("reason") == "discovery-no-eligible-work" else 1)
PY
  [ "$?" = 0 ] || fail "empty discovery abandon record has the wrong reason"
  printf '%s\n' "advance base after empty discovery" >> "$none/README.md"
  git -C "$none" add README.md
  git -C "$none" -c user.name=OMS -c user.email=oms@example.invalid commit -qm "advance base after empty discovery"
  rc=0
  run_autopilot "$none" propose --discover --planner claude \
    --allowed 'src,tests' --base main > "$none/rebind.out" 2>&1 || rc=$?
  [ "$rc" = 4 ] || fail "propose after base advance should bind a fresh receipt, got $rc: $(tail -5 "$none/rebind.out")"
  grep -Fq 'plan-from-spec: proposed' "$none/rebind.out" ||
    fail "propose after base advance did not generate a new proposal"
  [ -f "$none/.oms/plan/autopilot-run.json" ] ||
    fail "propose after base advance did not leave a fresh receipt"

  unusable="$TMP/scratch-refusal-discover-unusable"
  make_repo "$unusable"
  mkdir -p "$unusable/calls"
  rc=0
  OMS_T_PLAN_RESULT=unextractable run_autopilot "$unusable" propose --discover \
    --planner claude --allowed 'src,tests' --base main > "$unusable/unusable.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "unusable discovery answer should keep exit 3, got $rc: $(tail -5 "$unusable/unusable.out")"
  grep -Fq 'the next propose may bind a fresh contract' "$unusable/unusable.out" ||
    fail "unusable discovery answer did not report the receipt retirement"
  [ ! -e "$unusable/.oms/plan/autopilot-run.json" ] ||
    fail "unusable discovery answer left a live outer receipt"
  grep -Fl '"reason":"discovery-no-proposal"' "$unusable"/.oms/plan/autopilot-run.*.abandoned.json >/dev/null 2>&1 ||
    fail "unusable discovery answer did not write a discovery-no-proposal abandon record"
  rc=0
  run_autopilot "$unusable" propose --discover --planner claude \
    --allowed 'src,docs' --base main > "$unusable/other-allowed.out" 2>&1 || rc=$?
  [ "$rc" = 4 ] || fail "propose with a different envelope after an unusable discovery should bind fresh, got $rc: $(tail -5 "$unusable/other-allowed.out")"
  [ -f "$unusable/.oms/plan/autopilot-run.json" ] ||
    fail "propose after an unusable discovery did not bind a new receipt"

  interrupted="$TMP/scratch-refusal-discover-interrupted"
  make_repo "$interrupted"
  mkdir -p "$interrupted/calls"
  rc=0
  OMS_T_PLAN_RC=2 run_autopilot "$interrupted" propose --discover --planner claude \
    --allowed 'src,tests' --base main > "$interrupted/interrupted.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "interrupted proposal should fail with status 2, got $rc"
  [ -f "$interrupted/.oms/plan/autopilot-run.json" ] ||
    fail "interrupted proposal did not leave its outer receipt"
  if python3 - "$interrupted/.oms/plan/autopilot-run.json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    raise SystemExit(0 if json.load(handle).get("stage") == "proposing" else 1)
PY
  then
    :
  else
    fail "interrupted proposal did not leave a proposing receipt"
  fi
  rc=0
  OMS_T_PLAN_RESULT=empty-discovery run_autopilot "$interrupted" propose --discover \
    --planner claude --allowed 'src,tests' --base main > "$interrupted/empty.out" 2>&1 || rc=$?
  [ "$rc" = 0 ] || fail "empty discovery retry should succeed, got $rc"
  [ ! -e "$interrupted/.oms/plan/autopilot-run.json" ] ||
    fail "empty discovery retry left a live outer receipt"

  rc=0
  OMS_T_PLAN_RC=2 run_autopilot "$interrupted" propose --discover --planner claude \
    --allowed 'src,tests' --base main > "$interrupted/interrupted-again.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "second interrupted proposal should fail with status 2, got $rc"
  [ -f "$interrupted/.oms/plan/autopilot-run.json" ] ||
    fail "second interrupted proposal did not leave its outer receipt"
  original_receipt="$TMP/interrupted-original-receipt"
  cp "$interrupted/.oms/plan/autopilot-run.json" "$original_receipt"
  interrupted_branch="$(git -C "$interrupted" branch --show-current)"
  git -C "$interrupted" switch -q main
  printf '%s\n' "advance base after interrupted discovery" >> "$interrupted/README.md"
  git -C "$interrupted" add README.md
  git -C "$interrupted" -c user.name=OMS -c user.email=oms@example.invalid commit -qm "advance base after interrupted discovery"
  git -C "$interrupted" switch -q "$interrupted_branch"
  rc=0
  run_autopilot "$interrupted" propose --discover --planner claude \
    --allowed 'src,docs' --base main > "$interrupted/changed-allowed.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "base-moved retry with a changed allowed envelope should be refused, got $rc"
  grep -Fq 'immutable contract changed (contract)' "$interrupted/changed-allowed.out" ||
    fail "base-moved changed-allowed retry did not preserve the immutable-contract refusal"
  cmp -s "$original_receipt" "$interrupted/.oms/plan/autopilot-run.json" ||
    fail "changed-allowed refusal modified the old proposing receipt"
  rc=0
  run_autopilot "$interrupted" propose --discover --planner claude \
    --allowed 'src,tests' --base main > "$interrupted/rebind.out" 2>&1 || rc=$?
  [ "$rc" = 4 ] || fail "base-moved interrupted discovery should bind a fresh proposal, got $rc: $(tail -5 "$interrupted/rebind.out")"
  grep -Fq 'retired stale proposing receipt after base moved' "$interrupted/rebind.out" ||
    fail "base-moved interrupted receipt retirement was not reported: $(tail -8 "$interrupted/rebind.out")"
  [ -f "$interrupted/.oms/plan/autopilot-run.json" ] ||
    fail "base-moved retry did not bind a fresh receipt"
  if python3 - "$interrupted/.oms/plan/autopilot-run.json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    raise SystemExit(0 if json.load(handle).get("stage") == "proposal-review" else 1)
PY
  then
    :
  else
    fail "base-moved retry did not leave a fresh proposal-review receipt"
  fi
  abandon_record=""
  for candidate in "$interrupted"/.oms/plan/autopilot-run.*.abandoned.json; do
    [ -f "$candidate" ] || continue
    if python3 - "$candidate" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    raise SystemExit(0 if json.load(handle).get("reason") == "proposing-base-moved" else 1)
PY
    then
      abandon_record="$candidate"
      break
    fi
  done
  [ -n "$abandon_record" ] || fail "base-moved receipt retirement did not write an abandon record"
  abandoned_archive="${abandon_record%.abandoned.json}.json"
  cmp -s "$original_receipt" "$abandoned_archive" ||
    fail "base-moved abandon did not preserve the exact old receipt bytes"
}

test_autopilot_acceptance_reuse
test_autopilot_runs_from_verified_scratch_and_allows_ordinary_propose
test_auto_collaboration
test_atomic_proposal_apply
test_autopilot_abandon_plan_guidance
test_autopilot_floor_consent
test_autopilot_orchestration

echo "autopilot-smoke: ok"
