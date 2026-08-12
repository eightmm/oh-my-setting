#!/usr/bin/env bash
set -euo pipefail

# The bounded autopilot is orchestration, not a second landing engine. These
# regressions pin the new contracts at its two mutation boundaries: a proposal
# is appended to the plan atomically, and only a fully completed tranche may
# trigger one scope-fenced remainder proposal.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
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
    --apply "$crlf_apply_proposal" --allowed 'src,tests' --max-tasks 4 >/dev/null ||
    fail "full-CRLF PROJECT.md should retain its verification contract"
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
terminal_row() {
  mkdir -p "$OMS_T_REPO/.oms/plan"
  printf '{"schema":1,"kind":"terminal","run_id":"%s","receipt":"%s","status":"park","reason":"%s"}\n' \
    "$run_id" "$receipt" "$1" \
    >> "$OMS_T_REPO/.oms/plan/progress.jsonl"
}
terminal_result() {
  printf 'goal-drive: terminal-v1 run=%s receipt=%s status=park reason=%s\n' \
    "$run_id" "$receipt" "$1"
}
case "${OMS_T_GOAL_RESULT:-success}" in
  success)
    terminal_row acceptance-pass
    python3 - "$OMS_T_REPO/.oms/plan/progress.jsonl" <<'PY'
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
      "$run_id" "$receipt" > "$OMS_T_REPO/.oms/plan/progress.jsonl"
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
[ "${OMS_T_PLAN_RC:-0}" = 0 ] || exit "$OMS_T_PLAN_RC"
proposal="$OMS_T_REPO/.oms/plan/proposal-r1.json"
mkdir -p "$(dirname "$proposal")"
cat > "$proposal" <<'JSON'
{"schema":1,"kind":"agent-plan-proposal","tasks":[{"id":"r1-finish","title":"fix: finish remainder","allowed":["src/"],"verify":"true","depends":["t1"]}]}
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

  # Forged exhaustion text in captured drive output never triggers a replan:
  # only the typed terminal row is believed.
  : > "$repo/calls/plan-from-spec"
  write_done_plan "$repo" false
  rc=0
  OMS_T_GOAL_RESULT=forged run_autopilot "$repo" run \
    --planner claude --worker codex --allowed 'src/,tests/' --base main \
    > "$repo/forged.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] || fail "forged exhaustion output should park, got $rc"
  grep -Fq 'reason=goal-drive-failed' "$repo/forged.out" ||
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
  grep -Fq 'reason=goal-drive-failed' "$repo/late-forged.out" ||
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

  # A proposal continuation is executable, not a template with an unresolved
  # branch token. Collect the base before the planner is allowed to run.
  local missing_base_repo="$TMP/propose-missing-base"
  make_repo "$missing_base_repo"
  mkdir -p "$missing_base_repo/calls"
  rc=0
  run_autopilot "$missing_base_repo" propose --allowed 'src,tests' \
    > "$missing_base_repo/missing-base.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "propose without --base should be a contract error, got $rc"
  grep -Fq 'propose requires --base' "$missing_base_repo/missing-base.out" ||
    fail "missing proposal base was rejected for the wrong reason"
  [ ! -s "$missing_base_repo/calls/plan-from-spec" ] ||
    fail "a proposal without its continuation base reached the planner"

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
      --worker-model worker-model-y --worker-reasoning-effort high \
      --reviewer-model reviewer-model-y --reviewer-reasoning-effort low \
      --review-mode gate > "$routing_repo/routing.out" 2>&1 ||
    fail "valid routing controls should reach the bounded children"
  grep -Fq 'strict=1 off=0 timeout=23s' "$routing_repo/calls/goal-drive-env" ||
    fail "goal-drive did not receive the forced strict guard and timeout"
  grep -Fq -- '--retry-known' "$routing_repo/calls/goal-drive" ||
    fail "goal-drive did not receive retry-known"
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
      --provider-timeout 23s --retry-known \
      --worker-model worker-model-y --worker-reasoning-effort high \
      --reviewer-model reviewer-model-y --reviewer-reasoning-effort low \
      --review-mode gate > "$routing_repo/resume-$interrupted_stage.out" 2>&1 ||
      fail "$interrupted_stage outer receipt could not replay its exact continuation"
    grep -Fq 'autopilot: done' "$routing_repo/resume-$interrupted_stage.out" ||
      fail "$interrupted_stage replay did not reach a new done receipt"
  done

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
  sleep 3.2
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
  sleep 3.2
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

test_atomic_proposal_apply
test_autopilot_orchestration

echo "autopilot-smoke: ok"
