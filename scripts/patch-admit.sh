#!/usr/bin/env bash
set -euo pipefail

# Admission gate for delegated patches. A worker's patch can be stale (built on
# a since-moved base), partial, or pass only under the worker's own assumptions.
# Before it touches the main tree, apply it in a throwaway worktree off the
# current HEAD and run a checks ladder: it must still APPLY, parse, and pass the
# project verification contract. Emit a compact admission report and a verdict;
# exit nonzero unless every gate passes. This is the trust boundary between
# peer-delegate (which produces patches) and landing them.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT_LIB="$ROOT/scripts/lib"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT_LIB/agent-memory-common.sh"
# shellcheck source=scripts/lib/agent-task-common.sh
. "$ROOT_LIB/agent-task-common.sh"
# shellcheck source=scripts/lib/harness-residue.sh
. "$ROOT_LIB/harness-residue.sh"
# shellcheck source=scripts/lib/oms-common.sh
. "$ROOT_LIB/oms-common.sh"
# shellcheck source=scripts/lib/peer-common.sh
. "$ROOT_LIB/peer-common.sh"
# shellcheck source=scripts/lib/work-journal.sh
. "$ROOT_LIB/work-journal.sh"

REPO="$PWD"
PATCH=""
VERIFY=""
REGRESSION_TEST=""
REGRESSION_SET=0
VERIFY_EXPLICIT=0
VERIFY_EXIT=""
VERIFICATION="none"
BASE_VERIFY=""
ML=0
ALLOW_VERIFIER_CHANGE=0
ALLOW_TEST_REDUCTION=0
ALLOW_FLOOR_CHANGE_REASON=""
FLOOR_CHANGE_USED=false
ALLOW_RESTRUCTURE=0
ACCEPT_ENCODING_LOSS=0
KEEP_WORKTREE=0
REPORT=""
PLAN_TASK=""
COVERS=""
SCOPE_ALLOWED=""
SCOPE_FORBIDDEN=""
ACCEPTANCE_FILES=""
PLAN_ID=""
COVERS_PLAN_ID=""
worktree_parent=""
worktree=""
worktree_created=0
floor_worktree_parent=""
floor_worktree=""
floor_worktree_created=0
regression_worktree_parent=""
regression_worktree=""
regression_worktree_created=0
cleanup_done=0
reference_operation=""
reference_scope_dir=""
reference_channel=""
reference_scope_digest=""
reference_ready_digest=""
reference_monitor=""
reference_state=""
managed_input=0
reference_possible_writes=0
reuse_binding=""
intake_owned=0

usage() {
  cat <<'EOF'
Usage: patch-admit.sh --patch FILE [options]

Apply a delegated patch in an isolated worktree off the current HEAD and run a
checks ladder before it is allowed onto the main tree.

Options:
  --patch FILE   Patch file to admit (required).
  --repo PATH    Target git repo (default: current directory).
  --verify CMD   Verification command run in the worktree after applying.
                 With --plan-task, defaults to and must match its stored verifier.
                 Default: scripts/check.sh fast when implemented; otherwise
                 an existing check.sh requires an explicit --verify CMD.
  --regression-test CMD  Prove CMD fails on base with patch tests and passes on the candidate.
  --ml           Prefer the ml-smoke verification mode when auto-detecting.
  --report FILE  Write the admission report here (default: .oms/artifacts/admit/).
  --plan-task ID  Enforce plan paths plus authenticated dispatch grants for its lease.
  --covers ID    Acceptance criterion this admission proves (repeatable).
                 Ids are validated against the runtime envelope before any
                 work; the receipt records executed verification separately
                 from structural admission, bound to the patch digest.
  --keep-worktree  Keep the worktree for inspection.
  --allow-verifier-change  Permit a patch that modifies the verify command's
                 own files, including assertion-only test additions.
                 The base-owned verification floor still runs.
  --allow-test-reduction  Admit a patch that net-removes test assertions or
                 deletes a test file (normally rejected: passing by deleting
                 the check is the other way to self-certify). The base-owned
                 verification floor still runs.
  --allow-restructure  Admit a patch that adds a new top-level file or moves a
                 file across directories (normally rejected when no scope is
                 supplied: nothing else says where the patch may write).
  --allow-floor-change REASON  Owner-only exception for an intentional baseline
                 contract change. Run and record the failing base floor, but
                 admit only if candidate verification and all other gates pass.
  --accept-encoding-loss  Accept detected non-ASCII loss or transliteration in patch hunks.
  -h, --help     Show this help.

Ladder: patch applies cleanly (not stale) -> changed files stay in scope ->
changed shell files parse (bash -n) and pass ShellCheck when installed ->
patch does not weaken the tests ->
candidate verification passes -> unchanged baseline checks also pass with
verification surfaces restored from HEAD. Newly invoked tests run on the
candidate; an available HEAD project suite supplies their baseline floor.
Only --allow-floor-change REASON can accept a failed baseline command, after
candidate verification passes. Exit 0 only if every gate passes. Without
--plan-task there is no declared scope, so the structural floor
stands in for it: keep the existing layout.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

cleanup() {
  [ "$cleanup_done" = 0 ] || return 0
  cleanup_done=1
  if [ -n "$reference_operation" ]; then
    local reference_outcome=1
    [ "$reference_possible_writes" = 1 ] || reference_outcome=refuse-no-write
    python3 "$ROOT_LIB/landing-reference.py" finish "$reference_state" "$reference_operation" "$reference_outcome" >/dev/null 2>&1 || true
  fi
  oms_reference_stop "$reference_state" >/dev/null 2>&1 || true
  if [ -n "$regression_worktree" ] && [ "$regression_worktree_created" = 1 ]; then
    oms_with_worktree_registry_lock "$REPO" git -C "$REPO" worktree remove --force "$regression_worktree" >/dev/null 2>&1 || true
  fi
  if [ -n "$regression_worktree_parent" ]; then
    rm -rf "$regression_worktree_parent"
  fi
  if [ -n "$floor_worktree" ] && [ "$floor_worktree_created" = 1 ] && [ "$KEEP_WORKTREE" = 0 ]; then
    oms_with_worktree_registry_lock "$REPO" git -C "$REPO" worktree remove --force "$floor_worktree" >/dev/null 2>&1 || true
  fi
  if [ -n "$floor_worktree_parent" ] && [ "$KEEP_WORKTREE" = 0 ]; then
    rm -rf "$floor_worktree_parent"
  fi
  if [ -n "$worktree" ] && [ "$worktree_created" = 1 ] && [ "$KEEP_WORKTREE" = 0 ]; then
    oms_with_worktree_registry_lock "$REPO" git -C "$REPO" worktree remove --force "$worktree" >/dev/null 2>&1 || true
  fi
  if [ -n "$worktree_parent" ] && [ "$KEEP_WORKTREE" = 0 ]; then
    rm -rf "$worktree_parent"
  fi
}
cleanup_signal() {
  local code="$1"
  trap - EXIT HUP INT TERM
  cleanup
  exit "$code"
}
trap cleanup EXIT
trap 'cleanup_signal 129' HUP
trap 'cleanup_signal 130' INT
trap 'cleanup_signal 143' TERM

while [ "$#" -gt 0 ]; do
  case "$1" in
    --patch) [ "$#" -ge 2 ] || fail "--patch requires a file"; PATCH="$2"; shift 2 ;;
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --verify) [ "$#" -ge 2 ] || fail "--verify requires a command"; VERIFY="$2"; VERIFY_EXPLICIT=1; shift 2 ;;
    --regression-test) [ "$#" -ge 2 ] || fail "--regression-test requires a command"; [ -n "$2" ] || fail "--regression-test requires a nonempty command"; REGRESSION_TEST="$2"; REGRESSION_SET=1; shift 2 ;;
    --ml) ML=1; shift ;;
    --report) [ "$#" -ge 2 ] || fail "--report requires a path"; REPORT="$2"; shift 2 ;;
    --plan-task) [ "$#" -ge 2 ] || fail "--plan-task requires id"; PLAN_TASK="$2"; shift 2 ;;
    --covers) [ "$#" -ge 2 ] || fail "--covers requires a criterion id"; COVERS="$COVERS $2"; shift 2 ;;
    --executor) fail "Soul executors were removed; use a fresh plan task and brief" ;;
    --keep-worktree) KEEP_WORKTREE=1; shift ;;
    --allow-verifier-change) ALLOW_VERIFIER_CHANGE=1; shift ;;
    --allow-test-reduction) ALLOW_TEST_REDUCTION=1; shift ;;
    --allow-floor-change)
      [ "$#" -ge 2 ] || fail "--allow-floor-change requires a reason"
      [ "${OMS_HARNESS_CHILD:-0}" != 1 ] || fail "--allow-floor-change is parent-only"
      case "$2" in *$'\n'*|*$'\r'*) fail "--allow-floor-change requires a single-line reason" ;; esac
      printf '%s' "$2" | LC_ALL=C grep -E '[^[:space:]]' > /dev/null || fail "--allow-floor-change requires a nonblank reason"
      if printf '%s' "$2" | LC_ALL=C grep -E '[[:cntrl:]]' > /dev/null; then
        fail "--allow-floor-change reason cannot contain control characters"
      fi
      ALLOW_FLOOR_CHANGE_REASON="$2"; shift 2 ;;
    --allow-restructure) ALLOW_RESTRUCTURE=1; shift ;;
    --accept-encoding-loss) ACCEPT_ENCODING_LOSS=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ -n "$PATCH" ] || fail "--patch is required"
[ -f "$PATCH" ] || fail "patch not found: $PATCH"
REPO="$(cd "$REPO" && pwd -P)" || fail "bad --repo"
git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || fail "not a git repo: $REPO"
PATCH="$(cd "$(dirname "$PATCH")" && pwd)/$(basename "$PATCH")"
reference_state="$(oms_state_root "$REPO")" || fail "cannot resolve reference state"
managed_input="$(python3 "$ROOT_LIB/landing-reference.py" admit "$reference_state" "$PATCH" "${REPORT:-$reference_state/.oms/artifacts/admit/pending.md}")" || fail "unsafe managed admission input"
# Parent admissions with the default internal report retain the exact private
# generation for a later landing. Explicit reports and child calls keep their
# existing input/report contract and simply require fresh landing admission.
if [ "$managed_input" = 0 ] && [ -z "$REPORT" ] &&
  [ "${OMS_HARNESS_CHILD:-0}" != 1 ] && [ "${OMS_HARNESS_DELEGATE_DEPTH:-0}" = 0 ]; then
  agent_memory_ensure_oms_ignore "$reference_state" || fail "cannot initialize private admission state"
  intake_sha="$(oms_sha256_file "$PATCH")" || fail "cannot hash admission input"
  intake_id="land-admit-$(date -u +%Y%m%dT%H%M%SZ)-$$-${RANDOM:-0}"
  capture_rc=0
  PATCH="$(oms_with_file_lock "$reference_state/.oms/landings.jsonl" python3 "$ROOT_LIB/landing-capture.py" \
    allocate "$reference_state" "$PATCH" "$intake_id" "$intake_sha" "$REPO")" || capture_rc=$?
  [ "$capture_rc" -ne 3 ] || exit 1
  [ "$capture_rc" -eq 0 ] || fail "cannot capture admission input"
  managed_input=1
  intake_owned=1
fi
oms_reference_start "$reference_state" patch-admit "$PATCH" || fail "cannot start reference operation"
if [ "$managed_input" = 1 ] && [ -z "$REPORT" ]; then
  REPORT="$reference_state/.oms/artifacts/admit/admit-$reference_operation.md"
fi
oms_git_assert_safe_execution_config "$REPO" ||
  fail "unsafe executable Git config; remove it before patch admission"
oms_git_assert_plain_index "$REPO" ||
  fail "Git index has hidden or unreadable entries; clear them before patch admission"
# Everything below may establish plan lineage, invoke verification, or publish
# evidence. Any interrupted/failed path from here has an uncertain write outcome.
reference_possible_writes=1
if [ -n "$COVERS" ]; then
  ma_validate_covers_ids "$REPO" "$COVERS" || exit 2
  COVERS_PLAN_ID="${OMS_INDEX_PLAN_ID:-}"
fi

if [ -n "$PLAN_TASK" ]; then
  case "$PLAN_TASK" in *[!A-Za-z0-9._-]*|"") fail "--plan-task must match [A-Za-z0-9._-]+" ;; esac
  plan_json="$("$ROOT/scripts/agent-plan.sh" --repo "$REPO" evidence-snapshot --id "$PLAN_TASK")" || fail "cannot read plan task $PLAN_TASK"
  oms_require_current_task_contract "$plan_json" || exit $?
  PLAN_ID="$(printf '%s' "$plan_json" | python3 -c 'import json,sys;print(json.load(sys.stdin).get("plan_id", ""))')"
  if [ -z "$PLAN_ID" ]; then
    "$ROOT/scripts/agent-plan.sh" --repo "$REPO" ensure-lineage >/dev/null ||
      fail "cannot establish plan lineage before admitting task $PLAN_TASK"
    plan_json="$("$ROOT/scripts/agent-plan.sh" --repo "$REPO" evidence-snapshot --id "$PLAN_TASK")" ||
      fail "cannot reread plan task $PLAN_TASK after establishing lineage"
    PLAN_ID="$(printf '%s' "$plan_json" | python3 -c 'import json,sys;print(json.load(sys.stdin).get("plan_id", ""))')"
  fi
  case "$PLAN_ID" in
    plan_*)
      case "${PLAN_ID#plan_}" in *[!0-9a-f]*) fail "plan task has malformed lineage" ;; esac
      [ "${#PLAN_ID}" -eq 37 ] || fail "plan task has malformed lineage"
      ;;
    *) fail "plan task has no immutable lineage" ;;
  esac
  [ -z "$COVERS_PLAN_ID" ] || [ "$COVERS_PLAN_ID" = "$PLAN_ID" ] ||
    fail "plan changed between criterion validation and task snapshot"
  SCOPE_ALLOWED="$(printf '%s' "$plan_json" | python3 -c '
import json, sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
from panel_results import effective_allowed_paths
allowed = effective_allowed_paths(Path(sys.argv[2]), json.load(sys.stdin))
print(json.dumps(allowed) if allowed else "")
' "$ROOT_LIB" "$reference_state")" || fail "cannot read dispatch scope for plan task $PLAN_TASK"
  SCOPE_FORBIDDEN="$(printf '%s' "$plan_json" | python3 -c 'import json,sys;paths=json.load(sys.stdin).get("forbidden_paths",[]);print(json.dumps(paths) if paths else "")')"
  ACCEPTANCE_FILES="$(printf '%s' "$plan_json" | python3 -c 'import json,sys; d=json.load(sys.stdin); c=d.get("project_contract") or {}; print("\n".join(c.get("acceptance_files") or d.get("acceptance_files") or []))')"
  read_task_verify() {
    printf '%s' "$plan_json" | python3 -c '
import json,sys
value=json.load(sys.stdin).get("verify", "")
if not isinstance(value, str) or "\x00" in value:
    raise SystemExit(1)
sys.stdout.buffer.write((value + "__OMS_VERIFY_END__").encode("utf-8"))
'
  }
  task_verify_raw="$(read_task_verify)" || fail "cannot read task verify contract"
  case "$task_verify_raw" in
    *"__OMS_VERIFY_END__") ;;
    *) fail "cannot decode task verify contract" ;;
  esac
  task_verify="${task_verify_raw%__OMS_VERIFY_END__}"
  [ -n "$task_verify" ] || fail "plan task $PLAN_TASK has no stored verify contract"
  [ "$VERIFY_EXPLICIT" = 0 ] || [ "$VERIFY" = "$task_verify" ] ||
    fail "--verify does not match plan task $PLAN_TASK review contract"
  VERIFY="$task_verify"
  export OMS_TASK_ID="$PLAN_TASK" OMS_INDEX_PLAN_ID="$PLAN_ID"
fi

# Reuse and execution derive the same obligation from canonical plan/batch
# records. This also covers patch-land calls made directly during recovery.
batch_prefix='{"batches":[],"checks":[]}'
prefix_count=0
if [ -n "$PLAN_TASK" ]; then
  batch_prefix="$(python3 "$ROOT_LIB/landing-capture.py" batch-prefix "$reference_state" "$REPO" "$PLAN_TASK")" ||
    fail "cannot validate batch prefix for admission"
  prefix_count="$(printf '%s' "$batch_prefix" | python3 -c 'import json,sys;print(len(json.load(sys.stdin)["checks"]))')" ||
    fail "cannot read batch prefix verification count"
fi
prefix_field() {
  printf '%s' "$batch_prefix" | python3 -c '
import json,sys
value=json.load(sys.stdin)["checks"][int(sys.argv[1])][sys.argv[2]]
sys.stdout.buffer.write((value + "__OMS_PREFIX_END__").encode("utf-8"))
' "$1" "$2"
}

base_full_sha="$(git -C "$REPO" rev-parse HEAD 2>/dev/null || true)"
base_sha="$(git -C "$REPO" rev-parse --short HEAD 2>/dev/null || echo 'no-commit')"
patch_sha="$(oms_sha256_stream < "$PATCH" 2>/dev/null | cut -c1-16)"
[ -n "$patch_sha" ] || patch_sha="nohash"
reuse_parameters=("$VERIFY" "$ML" "$PLAN_TASK" "$ALLOW_VERIFIER_CHANGE" "$ALLOW_TEST_REDUCTION"
  "$ALLOW_FLOOR_CHANGE_REASON" "$ALLOW_RESTRUCTURE" "$ACCEPT_ENCODING_LOSS")
if [ "$managed_input" = 1 ] && [ "$REGRESSION_SET" = 0 ]; then
  reuse_binding="$(python3 "$ROOT_LIB/landing-capture.py" binding "$reference_state" "$REPO" "${reuse_parameters[@]}")" ||
    reuse_binding=""
fi

# Verdict accumulator.
ladder=""
verdict="ADMIT"
record() {
  # record GATE STATUS DETAIL. Verdict is computed from the ladder at the end;
  # only FAIL rejects (SKIP is neutral).
  ladder="$ladder$(printf '%s\t%s\t%s\n' "$1" "$2" "$3")
"
}

# --- Gate 1: applies cleanly to current HEAD (staleness check) --------------
if git -C "$REPO" apply --check --binary "$PATCH" >/dev/null 2>&1; then apply_ok=1; else apply_ok=0; fi
if [ "$apply_ok" = 1 ]; then
  record "apply" "PASS" "patch applies cleanly to $base_sha"
else
  record "apply" "FAIL" "patch does not apply to $base_sha (stale or conflicting)"
fi

# --- Gate 1b: patch carries no secrets (added lines only) -------------------
# A patch that applies and verifies can still smuggle a credential onto the
# main tree. Scan the added lines with the shared sensitive regex before any
# worktree work. This is a landing-side mirror of the outbound scrubber.
secret_scan_status=0
secret_scan_report="$(agent_memory_sensitive_diff_scan "$PATCH")" || secret_scan_status=$?
case "$secret_scan_status" in
  0) record "secrets" "FAIL" "added lines contain sensitive-looking content: $secret_scan_report" ;;
  1) record "secrets" "PASS" "no sensitive-looking content in added lines" ;;
  *)
    echo "error: sensitive-content scan failed; patch admission blocked" >&2
    record "secrets" "FAIL" "sensitive-content scan failed"
    ;;
esac

changed_files=""
verify_out=""
floor_verify_out=""
floor_hint=""
if [ "$apply_ok" = 1 ]; then
  oms_harness_prune_stale_worktrees "$REPO" 0 >/dev/null 2>&1 || true
  worktree_parent="$(mktemp -d "${TMPDIR:-/tmp}/oh-my-setting-admit.XXXXXX")" || fail "mktemp failed"
  worktree="$worktree_parent/wt-${worktree_parent##*.}"
  oms_harness_mark_tmpdir "$worktree_parent" "$REPO" "$worktree"
  if GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null \
    GIT_CONFIG_NOSYSTEM=1 oms_with_worktree_registry_lock "$REPO" git -c core.hooksPath=/dev/null \
    -c core.fsmonitor=false -C "$REPO" worktree add --quiet --detach \
    "$worktree" "${base_full_sha:-HEAD}" >/dev/null 2>&1; then
    worktree_created=1
    # Local-only agent files (project-private.sh) are not in HEAD; the verify
    # command may read PROJECT.md, so seed them the same way delegate does.
    oms_seed_local_agent_files "$REPO" "$worktree"
    # Auto-detection belongs to the trusted base, not to the patch being
    # admitted. Otherwise deleting or rewriting check.sh can also rewrite the
    # command that is supposed to judge that patch.
    if [ -x "$worktree/scripts/check.sh" ]; then
      # An explicit candidate-only test can use the unchanged project suite as
      # its floor. An unusable default must not invalidate an explicit command.
      BASE_VERIFY="$(oms_default_verify_command "$worktree/scripts/check.sh" "$ML" 2>/dev/null)" || BASE_VERIFY=""
      if [ -z "$VERIFY" ]; then
        [ -n "$BASE_VERIFY" ] || oms_default_verify_command "$worktree/scripts/check.sh" "$ML" >/dev/null
        VERIFY="$BASE_VERIFY"
      fi
    fi
    # The apply into the worktree must succeed, otherwise the syntax/verify
    # gates below would run against the UNPATCHED tree and pass vacuously. Gate
    # 1 only checks apply against HEAD; the worktree tree state can still differ.
    if GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null \
      GIT_CONFIG_NOSYSTEM=1 git -c core.hooksPath=/dev/null \
      -c core.fsmonitor=false -C "$worktree" apply --binary "$PATCH" >/dev/null 2>&1; then
    # git apply --numstat is TAB-delimited (add<TAB>del<TAB>path); split on the
    # tab so a path containing spaces is not truncated (which would silently
    # skip its syntax/verifier check).
    # numstat names additions/destinations, while the applied worktree diff
    # with rename detection disabled exposes deleted/renamed source paths.
    # Union both so moving a forbidden file into an allowed path cannot hide
    # the forbidden source side of the patch.
    changed_files="$({
      git -C "$REPO" -c core.quotePath=false apply --numstat "$PATCH" 2>/dev/null | awk -F '\t' '{print $3}'
      GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null \
        GIT_CONFIG_NOSYSTEM=1 git -c core.fsmonitor=false -c diff.external= \
        -c core.quotePath=false -C "$worktree" diff --no-ext-diff --no-textconv --name-only \
        --no-renames HEAD -- 2>/dev/null
    } | LC_ALL=C sort -u)"

    # --- Gate 2: task path scope ----------------------------------
    scope_detail="$(OMS_CHANGED="$changed_files" OMS_ALLOWED="$SCOPE_ALLOWED" OMS_FORBIDDEN="$SCOPE_FORBIDDEN" \
      OMS_PATH_SCOPE_HELPER="$ROOT_LIB/path_scope.py" python3 - <<'PY'
import json, os, runpy
scope = runpy.run_path(os.environ["OMS_PATH_SCOPE_HELPER"])
classify = scope["classify"]
normalize = scope["normalize"]
# Split on LF only: str.splitlines() also breaks on U+2028/U+0085, which
# unquoted git paths may contain, turning one name into two scope-clean ones.
changed=[x for x in os.environ.get("OMS_CHANGED","").split("\n") if x]
allowed = [normalize(x) for x in json.loads(os.environ.get("OMS_ALLOWED") or "[]")]
forbidden = [normalize(x) for x in json.loads(os.environ.get("OMS_FORBIDDEN") or "[]")]
bad=[]
for path in changed:
    # quotePath=false leaves only control characters, quotes and backslashes
    # C-quoted, a spelling no scope pattern matches: fail closed.
    if path.startswith('"') and (allowed or forbidden):
        bad.append("unquotable path: " + path)
        continue
    verdict = classify(path, allowed, forbidden)
    if verdict == "forbidden": bad.append("forbidden: " + path)
    elif verdict == "outside": bad.append("outside allowed paths: " + path)
print("; ".join(bad))
PY
)"
    if [ -n "$scope_detail" ]; then
      record "scope" "FAIL" "$scope_detail"
    elif [ -n "$SCOPE_ALLOWED$SCOPE_FORBIDDEN" ]; then
      record "scope" "PASS" "changed files satisfy task scope"
    else
      record "scope" "SKIP" "no task scope supplied"
    fi

    # --- Gate 2b: structural floor when no scope constrains the paths -------
    if [ -z "$SCOPE_ALLOWED$SCOPE_FORBIDDEN" ]; then
      base_files="$worktree_parent/base-files.txt"
      GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null \
        GIT_CONFIG_NOSYSTEM=1 git -c core.fsmonitor=false -c core.quotePath=false \
        -C "$worktree" ls-tree -r --name-only HEAD > "$base_files" 2>/dev/null || true
      structure_detail="$(OMS_CHANGED="$changed_files" OMS_BASE_FILE="$base_files" OMS_WORKTREE="$worktree" python3 - <<'PY'
import os
changed=[x for x in os.environ.get("OMS_CHANGED","").split("\n") if x]
with open(os.environ["OMS_BASE_FILE"], errors="replace") as handle:
    base=set(x for x in handle.read().split("\n") if x)
wt=os.environ["OMS_WORKTREE"]
added, removed = [], []
bad=[]
for path in changed:
    # Still C-quoted (control characters, quotes, backslashes): the text is not
    # a filesystem name, so the layout checks below cannot judge it.
    if path.startswith('"'):
        bad.append("unquotable path: " + path)
        continue
    here=os.path.lexists(os.path.join(wt, path))
    if here and path not in base: added.append(path)
    elif not here and path in base: removed.append(path)
bad += ["new top-level file: " + p for p in added if "/" not in p]
for src in removed:
    for dst in added:
        if os.path.basename(src) == os.path.basename(dst) and os.path.dirname(src) != os.path.dirname(dst):
            bad.append("moved across directories: %s -> %s" % (src, dst))
print("; ".join(bad))
PY
)"
      if [ -z "$structure_detail" ]; then
        record "structure" "PASS" "changed files keep the existing layout"
      elif [ "$ALLOW_RESTRUCTURE" = 1 ]; then
        record "structure" "PASS" "restructure permitted by --allow-restructure: $structure_detail"
      else
        record "structure" "FAIL" "patch restructures the tree: $structure_detail (override: --allow-restructure)"
      fi
    else
      record "structure" "SKIP" "declared scope enforced instead"
    fi

    # --- Gate 3: changed syntax-checked files parse -------------------------
    syntax_ok=1
    syntax_checked=0
    shellcheck_bin="${OMS_SHELLCHECK_BIN:-shellcheck}"
    shellcheck_skipped=0
    syntax_detail="no syntax-checked files changed"
    while IFS= read -r f; do
      [ -n "$f" ] || continue
      case "$f" in
        *.sh)
          if [ -f "$worktree/$f" ]; then
            if ! bash -n "$worktree/$f" 2>/dev/null; then
              syntax_ok=0
              syntax_detail="bash -n failed: $f"
              break
            fi
            syntax_checked=1
            if command -v "$shellcheck_bin" >/dev/null 2>&1; then
              # Bash parsing supplies the dialect; omit source-context codes without following sources.
              if ! (cd "$worktree" && ma_run_bounded "${OMS_PEER_VERIFY_TIMEOUT:-10m}" verify "$shellcheck_bin" \
                -S warning -e SC1090,SC1091,SC2034,SC2148,SC2154 "$worktree/$f") \
                > "$worktree_parent/shellcheck.out" 2>&1; then
                syntax_ok=0
                shellcheck_detail="$(sed '/^[[:space:]]*$/d' "$worktree_parent/shellcheck.out" | sed -n '1,3p' | cut -c 1-200 | tr '\n' ' ')"
                syntax_detail="shellcheck failed: $f${shellcheck_detail:+ — $shellcheck_detail}"
                break
              fi
            elif [ "$shellcheck_skipped" = 0 ]; then
              record "shellcheck" "SKIP" "ShellCheck not installed"
              shellcheck_skipped=1
            fi
          fi
          ;;
        *.py)
          if [ -f "$worktree/$f" ] && command -v python3 >/dev/null 2>&1; then
            if ! python3 -m py_compile "$worktree/$f" 2>/dev/null; then
              syntax_ok=0
              syntax_detail="python compile failed: $f"
              break
            fi
            syntax_checked=1
          fi
          ;;
        *.json)
          if [ -f "$worktree/$f" ] && command -v python3 >/dev/null 2>&1; then
            if ! python3 -m json.tool "$worktree/$f" >/dev/null 2>&1; then
              syntax_ok=0
              syntax_detail="json parse failed: $f"
              break
            fi
            syntax_checked=1
          fi
          ;;
      esac
    done <<EOF
$changed_files
EOF
    if [ "$syntax_ok" = 1 ] && [ "$syntax_checked" = 1 ]; then
      syntax_detail="changed shell/python/json files parse"
    fi
    [ "$syntax_ok" = 1 ] && record "syntax" "PASS" "$syntax_detail" \
      || record "syntax" "FAIL" "$syntax_detail"

    # --- Gate 3a: identify the automatic verification surface ---------------
    verifier_hit=""
    verification_surface=""
    surface_scan=""
    surface_tab="$(printf "\t")"
    if [ -n "$VERIFY" ] && command -v python3 >/dev/null 2>&1; then
      scan_surface() {
        OMS_VERIFY="$VERIFY" OMS_BATCH_PREFIX="$batch_prefix" OMS_CHANGED="$changed_files" OMS_WORKTREE="$worktree" \
          OMS_ACCEPTANCE_FILES="$ACCEPTANCE_FILES" OMS_SELECTOR_LIB="$ROOT_LIB" python3 - <<'SURFACE_PY'
import json
import os
import posixpath
import re
import shlex
import subprocess

wt = os.environ["OMS_WORKTREE"]
def base(path):
    env = dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null", GIT_CONFIG_NOSYSTEM="1")
    result = subprocess.run(["git", "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false",
                             "-C", wt, "show", "HEAD:" + path],
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=env)
    return result.stdout if result.returncode == 0 else None

# Pure reader operands are data; tools that load programs need a broader surface.
READERS = {"grep", "egrep", "fgrep", "rg", "cat", "head", "tail", "sed", "awk", "gawk", "mawk", "nawk", "wc", "diff", "cmp",
           "test", "[", "jq", "ls", "stat", "file", "shasum", "md5sum", "cksum", "sum",
           "sha1sum", "sha224sum", "sha256sum", "sha384sum", "sha512sum", "b2sum"}

def resolve(token, cwd=""):
    if os.path.isabs(token):
        token = os.path.relpath(token, wt).replace(os.sep, "/")
    else:
        token = posixpath.join(cwd, token)
    path = posixpath.normpath(token)
    return path if path != ".." and not path.startswith("../") else ""

def system_command(token):
    # Repository paths retain their identity even when named like a wrapper,
    # interpreter or reader. Only bare commands and conventional system paths
    # get the argument semantics of those tools.
    return "/" not in token or (os.path.isabs(token) and
        posixpath.dirname(posixpath.normpath(token)) in ("/bin", "/usr/bin", "/usr/local/bin")
        and not resolve(token))

def program_files(executable, args, cwd):
    if executable not in ("awk", "gawk", "mawk", "nawk", "sed", "jq"):
        return
    # Option sets vary by tool/version. Every existing file operand is part
    # of the verifier, including data operands. Also unwrap file options,
    # including a program attached after f inside a single-dash bundle.
    for index, arg in enumerate(args):
        targets = [arg]
        for prefix in ("--file=", "--from-file="):
            if arg.startswith(prefix):
                targets.append(arg[len(prefix):])
        if arg in ("--file", "--from-file") and index + 1 < len(args):
            targets.append(args[index + 1])
        elif arg.startswith("-") and not arg.startswith("--") and "f" in arg[1:]:
            attached = arg.split("f", 1)[1]
            if attached:
                targets.append(attached)
            elif index + 1 < len(args):
                targets.append(args[index + 1])
        for target in targets:
            path = resolve(target, cwd)
            if path and (os.path.isfile(os.path.join(wt, path)) or base(path) is not None):
                named.add(path)

# Executed paths and program-tool file operands confer verifier authority.
# Arguments to grep, hashes or other content readers remain product inputs,
# even if their basename matches
# a script elsewhere. Do not evaluate shell expansions while classifying them.
named = set()
new_only = []
def commands(command, cwd="", candidate_eligible=True):
    try:
        lexer = shlex.shlex(command, posix=True, punctuation_chars=";&|()<>")
        lexer.whitespace_split = True
        tokens = list(lexer)
    except ValueError:
        return
    segments, segment = [], []
    for token in tokens:
        if token in (";", "&&", "||", "|", "&", "(", ")"):
            if segment:
                segments.append(segment)
            segment = []
        else:
            segment.append(token)
    if segment:
        segments.append(segment)
    for parts in segments:
        while parts and re.match(r"^[A-Za-z_][A-Za-z_0-9]*=", parts[0]):
            parts = parts[1:]
        # Peel wrappers so `timeout 10 bash x.sh`, `nice -n 5 x.sh` or `exec x.sh`
        # still name x.sh; a wrapped verifier must not escape consent or the floor.
        while parts:
            head = os.path.basename(parts[0])
            if system_command(parts[0]) and head in ("command", "env", "exec", "time", "nohup", "builtin", "stdbuf", "nice", "timeout"):
                parts = parts[1:]
                while parts and (parts[0] == "--" or parts[0].startswith("-")
                                 or re.match(r"^[A-Za-z_][A-Za-z_0-9]*=", parts[0])):
                    flag = parts[0]
                    parts = parts[1:]
                    if parts and ((head == "nice" and flag == "-n") or (head == "timeout" and flag in ("-s", "-k"))
                                  or (head == "stdbuf" and flag in ("-i", "-o", "-e"))):
                        parts = parts[1:]
                if head == "timeout" and parts:
                    parts = parts[1:]
                continue
            break
        if not parts:
            continue
        executable = os.path.basename(parts[0]) if system_command(parts[0]) else ""
        program_files(executable, parts[1:], cwd)
        if executable in (".", "source") and len(parts) > 1:
            path = resolve(parts[1], cwd)
            if path:
                named.add(path)
            continue
        if executable not in READERS:
            for operand in parts[1:]:
                if operand.endswith((".sh", ".bash", ".py")) and not operand.startswith("-"):
                    operand_path = resolve(operand, cwd)
                    if operand_path:
                        named.add(operand_path)
        if executable == "cd" and len(parts) == 2:
            cwd = resolve(parts[1], cwd)
            continue
        target = parts[0]
        if executable in ("bash", "sh", "python", "python3", "node", "ruby", "perl"):
            args = parts[1:]
            if "-c" in args and args.index("-c") + 1 < len(args):
                commands(args[args.index("-c") + 1], cwd, False)
                continue
            if "-m" in args or not args or args[0] == "-":
                continue
            if executable == "node" and "--test" in args:
                for arg in args:
                    if not arg.startswith("-"):
                        operand_path = resolve(arg, cwd)
                        if operand_path:
                            named.add(operand_path)
                target = ""
            else:
                target = next((arg for arg in args if not arg.startswith("-")), "")
        path = resolve(target, cwd) if target else ""
        if path:
            named.add(path)
            if (candidate_eligible and len(segments) == 1 and not cwd and "<" not in parts and ">" not in parts
                    and base(path) is None and os.path.isfile(os.path.join(wt, path))):
                new_only.append(path)
commands(os.environ["OMS_VERIFY"])
for check in json.loads(os.environ["OMS_BATCH_PREFIX"])["checks"]:
    commands(check["verify"], candidate_eligible=False)
reviewed = set(x for x in os.environ.get("OMS_ACCEPTANCE_FILES", "").split("\n") if x)
ENTRY = {"check.sh", "Makefile", "makefile", "GNUmakefile", "package.json",
         "pyproject.toml", "tox.ini", "noxfile.py", "conftest.py",
         "setup.py", "setup.cfg", "justfile", "Justfile"}
HELPER_PATH = re.compile(
    r"(^|/)(?:run[-_.])?(?:checks?|tests?|verify|lint)"
    r"(?:[-_.][^/]*)?\.(?:sh|bash|py|js|cjs|mjs|ts|tsx|rb|pl|ps1)$", re.IGNORECASE)
TEST_PATH = re.compile(
    r"(^|/)(tests?|spec)(/|$)|(^|/)test_[^/]+$|"
    r"[^/]*(_test|_spec|\.test|\.spec)\.[^/.]+$")
for f in os.environ["OMS_CHANGED"].split("\n"):
    if not f:
        continue
    direct = f in named
    common = os.path.basename(f) in ENTRY or bool(HELPER_PATH.search(f))
    explicit = f in reviewed
    old = base(f)
    if direct or common or explicit or TEST_PATH.search(f):
        print("surface\t" + f)
        if old is not None:
            print("existing\t" + f)
    if (direct or common or explicit) and old is not None:
        print("verifier\t" + f)
if len(new_only) == 1:
    print("candidate-only\t" + new_only[0] + " (file added by patch)")
else:
    import sys
    sys.path.insert(0, os.environ["OMS_SELECTOR_LIB"])
    from verify_selectors import candidate_only_targets
    targets = candidate_only_targets(wt, os.environ["OMS_VERIFY"],
                                     set(os.environ["OMS_CHANGED"].split("\n")))
    if targets:
        print("candidate-only\t" + "; ".join(targets))
SURFACE_PY
      }
      surface_scan="$(scan_surface)"
      surface_tab="$(printf '\t')"
      verifier_hit="$(printf '%s\n' "$surface_scan" | awk -F "$surface_tab" '$1 == "verifier" {print substr($0, length($1) + 2); exit}')"
      verification_surface="$(printf '%s\n' "$surface_scan" | awk -F "$surface_tab" '$1 == "surface" {print substr($0, length($1) + 2)}')"
    fi
    floor_verify="$VERIFY"
    surface_rows() {
      printf '%s\n' "$surface_scan" | awk -F "$surface_tab" -v kind="$1" '$1 == kind {print $2}'
    }
    existing_surface="$(surface_rows existing)"
    candidate_only="$(surface_rows candidate-only)"
    if [ -n "$candidate_only" ]; then
      floor_verify="$BASE_VERIFY"
      record "candidate-only" "PASS" "$candidate_only"
    fi
    # --- Gate 3b: the patch must not quietly weaken the tests ---------------
    test_path_pattern='(^|/)(tests?|spec)/|(^|/)test_[^/]+$|[^/]*(_test|_spec|\.test|\.spec)\.[^/.]+$'
    test_reduction=""
    if [ "$ALLOW_TEST_REDUCTION" = 0 ] && command -v python3 >/dev/null 2>&1; then
      test_reduction="$(OMS_TEST_PATH_PATTERN="$test_path_pattern" OMS_PATCH="$PATCH" python3 - <<'PY'
import os
import re

TEST_PATH = re.compile(os.environ["OMS_TEST_PATH_PATTERN"])
ASSERT = re.compile(
    r"\bassert|\bexpect\s*\(|\bshould\b|\bfail\b|t\.(Error|Fatal)|"
    r"\bok\s*\(|\brequire\s*\.|\bXCTAssert"
)

path = None
counted = False
added = removed = 0
deleted_files = []
try:
    with open(os.environ["OMS_PATCH"], errors="replace") as handle:
        lines = handle.read().splitlines()
except OSError:
    raise SystemExit(0)

for line in lines:
    if line.startswith("diff --git "):
        parts = line.split(" b/", 1)
        path = parts[1].strip() if len(parts) == 2 else None
        counted = bool(path and TEST_PATH.search(path))
        continue
    if line.startswith("deleted file mode") and counted and path:
        deleted_files.append(path)
        continue
    if not counted or line.startswith(("+++", "---", "@@")):
        continue
    if line.startswith("+") and ASSERT.search(line):
        added += 1
    elif line.startswith("-") and ASSERT.search(line):
        removed += 1

if deleted_files:
    print("deleted test file %s" % deleted_files[0])
elif removed > added:
    print("%d assertion line(s) removed, %d added" % (removed, added))
PY
)"
    fi
    if [ -n "$test_reduction" ]; then
      record "tests" "FAIL" "patch weakens the tests: $test_reduction (override: --allow-test-reduction)"
    else
      record "tests" "PASS" "patch does not net-remove test assertions"
    fi

    comma_join() {
      printf '%s\n' "$1" | awk 'NF { printf "%s%s", separator, $0; separator=", " } END { if (separator != "") print "" }'
    }
    encoding_loss="$(OMS_PATCH="$PATCH" OMS_SALVAGE_LIB="$ROOT_LIB" python3 - <<'PY'
import os
import sys
sys.path.insert(0, os.environ['OMS_SALVAGE_LIB'])
from panel_salvage import patch_encoding_warnings
with open(os.environ['OMS_PATCH'], 'rb') as handle:
    warnings = patch_encoding_warnings(handle.read())
seen = set()
for warning in warnings:
    item = '%s:%s' % (warning['file'], warning.get('hunk') or '<unknown hunk>')
    if item not in seen:
        print(item)
        seen.add(item)
PY
)"
    if [ -n "$encoding_loss" ]; then
      encoding_detail="$(comma_join "$encoding_loss")"
      if [ "$ACCEPT_ENCODING_LOSS" = 0 ]; then
        record "encoding-loss" "FAIL" "patch hunks lose non-ASCII text: $encoding_detail (override: --accept-encoding-loss)"
      else
        record "encoding-loss" "PASS" "encoding loss accepted for: $encoding_detail"
      fi
    else
      record "encoding-loss" "PASS" "no encoding loss detected in patch hunks"
    fi

    surface_detail="$(comma_join "$verification_surface")"
    if [ -n "$verifier_hit" ] && [ "$ALLOW_VERIFIER_CHANGE" = 0 ]; then
      record "verifier" "FAIL" "patch modifies its own verifier: $verifier_hit (override: --allow-verifier-change; base floor still required; changed shell/python/json files are already parsed by the syntax gate, so drop bash -n or py_compile checks of them from --verify)"
    elif [ -n "$verifier_hit" ]; then
      record "verifier" "PASS" "verifier change permitted; base floor still required: $verifier_hit"
    elif [ -n "$verification_surface" ]; then
      record "verifier" "PASS" "verification surface change requires base floor: $surface_detail"
    elif [ -n "$VERIFY" ]; then
      record "verifier" "PASS" "patch leaves the verification surface untouched"
    fi

    preverify_failed=0
    case "$ladder" in *"	FAIL	"*) preverify_failed=1 ;; esac
    prefix_failed=0
    prefix_index=0
    while [ "$prefix_index" -lt "$prefix_count" ]; do
      prefix_id_raw="$(prefix_field "$prefix_index" id)" || fail "cannot read batch prefix task"
      prefix_verify_raw="$(prefix_field "$prefix_index" verify)" || fail "cannot read batch prefix verifier"
      prefix_id="${prefix_id_raw%__OMS_PREFIX_END__}"
      prefix_verify="${prefix_verify_raw%__OMS_PREFIX_END__}"
      if [ "$preverify_failed" = 1 ] || [ "$prefix_failed" = 1 ]; then
        record "verify-prefix-$prefix_id" "SKIP" "earlier policy, integrity or prefix gate failed"
      elif prefix_started="$(date +%s)" && prefix_out="$(cd "$worktree" && OMS_TASK_ID="$prefix_id" OMS_PEER_VERIFY_TIMEOUT="${OMS_PEER_VERIFY_TIMEOUT:-20m}" run_verify_with_timeout bash -c "$prefix_verify" 2>&1)"; then
        record "verify-prefix-$prefix_id" "PASS" "$prefix_verify — $(( $(date +%s) - prefix_started ))s"
      else
        prefix_exit=$?
        prefix_failed=1
        timeout_note="$(OMS_PEER_VERIFY_TIMEOUT="${OMS_PEER_VERIFY_TIMEOUT:-20m}" ma_verify_timeout_note "$prefix_exit")"
        record "verify-prefix-$prefix_id" "FAIL" "$prefix_verify${timeout_note:+ — $timeout_note}"
        echo "patch-admit: earlier task $prefix_id verifier failed; candidate refused" >&2
        verify_out="$prefix_out"
      fi
      prefix_index=$((prefix_index + 1))
    done
    if [ "$REGRESSION_SET" = 1 ]; then
      regression_result="inconclusive: setup failed"
      regression_base_exit="not-run"
      regression_candidate_exit="not-run"
      regression_base_digest="unavailable"
      regression_candidate_digest="unavailable"
      regression_base_out=""
      regression_candidate_out=""
      if [ "$preverify_failed" = 1 ] || [ "$prefix_failed" = 1 ]; then
        regression_result="inconclusive: earlier gate failed"
      elif regression_worktree_parent="$(mktemp -d "${TMPDIR:-/tmp}/oh-my-setting-regression.XXXXXX")"; then
        regression_worktree="$regression_worktree_parent/wt"
        oms_harness_mark_tmpdir "$regression_worktree_parent" "$REPO" "$regression_worktree"
        if GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_NOSYSTEM=1 \
          oms_with_worktree_registry_lock "$REPO" git -c core.hooksPath=/dev/null -c core.fsmonitor=false \
          -C "$REPO" worktree add --quiet --detach "$regression_worktree" "$base_full_sha" >/dev/null 2>&1; then
          regression_worktree_created=1
          oms_seed_local_agent_files "$REPO" "$regression_worktree"
          if OMS_TEST_PATH_PATTERN="$test_path_pattern" OMS_REGRESSION_PATCH="$PATCH" \
            OMS_REGRESSION_DIR="$regression_worktree_parent" OMS_REGRESSION_REPO="$regression_worktree" \
            python3 - <<'PY_REGRESSION'
import os, re, subprocess
from pathlib import Path
root = os.environ["OMS_REGRESSION_REPO"]
tmp = Path(os.environ["OMS_REGRESSION_DIR"])
env = os.environ.copy()
env.update(GIT_INDEX_FILE=str(tmp / "index"), GIT_CONFIG_GLOBAL="/dev/null",
           GIT_CONFIG_SYSTEM="/dev/null", GIT_CONFIG_NOSYSTEM="1")
def git(*args):
    return subprocess.check_output(["git", "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false", "-C", root, *args], env=env, stderr=subprocess.DEVNULL)
git("read-tree", "HEAD")
git("apply", "--cached", "--binary", os.environ["OMS_REGRESSION_PATCH"])
(tmp / "candidate-digest").write_bytes(git("write-tree"))
paths = git("diff", "--cached", "--name-only", "-z", "--no-renames", "HEAD").split(b"\0")
pattern = re.compile(os.environ["OMS_TEST_PATH_PATTERN"])
paths = [os.fsdecode(p) for p in paths if p and pattern.search(os.fsdecode(p))]
if not paths:
    raise SystemExit(1)
patch = git("diff", "--cached", "--binary", "--no-renames", "HEAD", "--", *(":(literal)" + p for p in paths))
(tmp / "tests.patch").write_bytes(patch)
git("read-tree", "HEAD")
git("apply", "--cached", "--binary", str(tmp / "tests.patch"))
(tmp / "base-digest").write_bytes(git("write-tree"))
PY_REGRESSION
          then
            regression_base_digest="$(cat "$regression_worktree_parent/base-digest")"
            regression_candidate_digest="$(cat "$regression_worktree_parent/candidate-digest")"
            if GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_NOSYSTEM=1 \
              git -c core.hooksPath=/dev/null -c core.fsmonitor=false -C "$regression_worktree" apply --binary "$regression_worktree_parent/tests.patch" >/dev/null 2>&1; then
              regression_run() {
                local regression_dir="$1" regression_status="$2"
                cd "$regression_dir" || return 125
                OMS_PEER_VERIFY_TIMEOUT="${OMS_PEER_VERIFY_TIMEOUT:-20m}" run_verify_with_timeout \
                  python3 -c 'import subprocess,sys
r = subprocess.run(["bash", "-c", sys.argv[1]])
with open(sys.argv[2], "w") as f:
    f.write("signal" if r.returncode < 0 else "exit")
sys.exit(128 - r.returncode if r.returncode < 0 else r.returncode)' \
                  "$REGRESSION_TEST" "$regression_status"
              }
              regression_base_exit=0
              regression_base_out="$(regression_run "$regression_worktree" "$regression_worktree_parent/base-status" 2>&1 | tail -n 20)" || regression_base_exit=$?
              regression_candidate_exit=0
              regression_candidate_out="$(regression_run "$worktree" "$regression_worktree_parent/candidate-status" 2>&1 | tail -n 20)" || regression_candidate_exit=$?
              regression_base_kind="$(cat "$regression_worktree_parent/base-status" 2>/dev/null || :)"
              regression_candidate_kind="$(cat "$regression_worktree_parent/candidate-status" 2>/dev/null || :)"
              if [ "$regression_base_kind" != exit ] || [ "$regression_candidate_kind" != exit ] ||
                { [ "$regression_base_exit" -ge 124 ] && [ "$regression_base_exit" -le 127 ]; } ||
                { [ "$regression_candidate_exit" -ge 124 ] && [ "$regression_candidate_exit" -le 127 ]; }; then
                regression_result="inconclusive: command did not exit normally"
              elif [ "$regression_candidate_exit" -ne 0 ]; then
                regression_result="failing-after"
              elif [ "$regression_base_exit" -eq 0 ]; then
                regression_result="not-failing-before"
              else
                regression_result="proven"
              fi
            fi
          else
            regression_result="inconclusive: cannot construct test-only base"
          fi
        fi
      fi
      case "$regression_result" in
        proven) record "regression" "PASS" "$regression_result" ;;
        failing-after|not-failing-before) record "regression" "FAIL" "$regression_result" ;;
        *) record "regression" "SKIP" "$regression_result" ;;
      esac
    fi

    if [ -n "$VERIFY" ]; then
      if [ "$preverify_failed" = 1 ]; then
        record "verify" "SKIP" "pre-verification policy or integrity gate failed"
      elif [ "$prefix_failed" = 1 ]; then
        record "verify" "SKIP" "earlier task prefix verification failed"
      else
        verify_started="$(date +%s)"
        if verify_out="$(cd "$worktree" && OMS_PEER_VERIFY_TIMEOUT="${OMS_PEER_VERIFY_TIMEOUT:-20m}" run_verify_with_timeout bash -c "$VERIFY" 2>&1)"; then
          VERIFICATION="command"
          VERIFY_EXIT=0
        else
          VERIFY_EXIT=$?
          VERIFICATION="command"
        fi
        verify_seconds=$(( $(date +%s) - verify_started ))
        if [ "$VERIFY_EXIT" -eq 0 ]; then
          record "verify" "PASS" "$VERIFY — ${verify_seconds}s"
        else
          timeout_note="$(OMS_PEER_VERIFY_TIMEOUT="${OMS_PEER_VERIFY_TIMEOUT:-20m}" ma_verify_timeout_note "$VERIFY_EXIT")"
          record "verify" "FAIL" "$VERIFY — ${verify_seconds}s${timeout_note:+ — $timeout_note}"
        fi
      fi
    else
      record "verify" "SKIP" "no --verify and no base scripts/check.sh"
    fi

    # Informational: name affected suites that --verify skips so they do not
    # first fail at the land gate. The graph needs a Git range, so the applied
    # tree becomes an unreferenced commit via a throwaway index; the trusted
    # harness graph reads it, never the candidate's scripts. Fail-open. The
    # graph diffs the worktree, so a racy index entry re-hashes files: keep
    # global/system filters and fsmonitor out, like every git call above.
    if [ "$VERIFY_EXIT" = 0 ] && [ "$VERIFICATION" = command ] &&
      [ -x "$ROOT/scripts/graph.sh" ] && command -v timeout >/dev/null 2>&1; then
      hint_idx="$worktree_parent/hint-index"
      hint_git() { GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_NOSYSTEM=1 git -c core.fsmonitor=false -c core.hooksPath=/dev/null -C "$worktree" "$@"; }
      hint_tests=""
      if GIT_INDEX_FILE="$hint_idx" hint_git read-tree HEAD >/dev/null 2>&1 &&
        GIT_INDEX_FILE="$hint_idx" hint_git add -A >/dev/null 2>&1 &&
        hint_tree="$(GIT_INDEX_FILE="$hint_idx" hint_git write-tree 2>/dev/null)" &&
        hint_head="$(hint_git rev-parse HEAD 2>/dev/null)" &&
        hint_commit="$(GIT_AUTHOR_NAME=oms GIT_AUTHOR_EMAIL=oms@localhost GIT_COMMITTER_NAME=oms GIT_COMMITTER_EMAIL=oms@localhost \
          hint_git commit-tree "$hint_tree" -p "$hint_head" -m admit 2>/dev/null)" &&
        hint_json="$(GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_NOSYSTEM=1 \
          GIT_CONFIG_COUNT=2 GIT_CONFIG_KEY_0=core.fsmonitor GIT_CONFIG_VALUE_0=false \
          GIT_CONFIG_KEY_1=core.hooksPath GIT_CONFIG_VALUE_1=/dev/null \
          timeout 60 bash "$ROOT/scripts/graph.sh" project affected --repo "$worktree" \
          --base "$hint_head" --head "$hint_commit" \
          --guard 'scripts/graph.sh' --guard 'scripts/lib/oms_graph/**' --guard 'tests/graph-smoke.sh' \
          --runnable 'tests/operator/*' --json 2>/dev/null)"; then
        hint_tests="$(printf '%s' "$hint_json" | OMS_VERIFY_CMD="$VERIFY" python3 -c '
import json, os, sys
try:
    tests = json.load(sys.stdin).get("tests") or []
except Exception:
    sys.exit(0)
verify = os.environ.get("OMS_VERIFY_CMD", "")
for t in tests:
    path = t.get("path") if isinstance(t, dict) else t
    if isinstance(path, str) and path and path not in verify:
        print(path)
' 2>/dev/null)" || hint_tests=""
      fi
      rm -f "$hint_idx"
      if [ -n "$hint_tests" ]; then
        hint_n="$(printf '%s\n' "$hint_tests" | grep -c .)"
        echo "note: land will also run $hint_n affected suite(s) not in --verify: $(printf '%s\n' "$hint_tests" | head -5 | tr '\n' ' ' | sed 's/ $//')" >&2
      fi
    fi

    # If any automatic verification surface changed, apply the complete patch
    # to a second worktree at the same HEAD, restore only that surface from the
    # base, and run the baseline command. Product changes remain in place, so
    # a helper that made a broken candidate pass fails under base-owned checks.
    if [ "$preverify_failed" = 1 ]; then
      record "verify-floor" "SKIP" "pre-verification policy or integrity gate failed"
    elif [ -n "$candidate_only" ] && [ -z "$floor_verify" ]; then
      if [ -n "$existing_surface" ]; then
        record "verify-floor" "FAIL" "candidate-only command cannot verify changed baseline surfaces; and HEAD scripts/check.sh exposes no fast/ml-smoke floor; add one at HEAD, or rerun with --verify naming an existing suite that covers them"
      else
        record "verify-floor" "SKIP" "candidate-only test; no changed baseline verification surface"
      fi
    elif [ -n "$floor_verify" ] && [ -n "$verification_surface" ]; then
      if floor_worktree_parent="$(mktemp -d "${TMPDIR:-/tmp}/oh-my-setting-admit.XXXXXX")"; then
        floor_worktree="$floor_worktree_parent/wt-${floor_worktree_parent##*.}"
        oms_harness_mark_tmpdir "$floor_worktree_parent" "$REPO" "$floor_worktree"
        if GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null \
          GIT_CONFIG_NOSYSTEM=1 oms_with_worktree_registry_lock "$REPO" git -c core.hooksPath=/dev/null \
          -c core.fsmonitor=false -C "$REPO" worktree add --quiet --detach \
          "$floor_worktree" "${base_full_sha:-HEAD}" >/dev/null 2>&1; then
          floor_worktree_created=1
          oms_seed_local_agent_files "$REPO" "$floor_worktree"
          if GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null \
            GIT_CONFIG_NOSYSTEM=1 git -c core.hooksPath=/dev/null \
            -c core.fsmonitor=false -C "$floor_worktree" apply --binary "$PATCH" >/dev/null 2>&1; then
            floor_restore_ok=1
            floor_restore_detail=""
            while IFS= read -r f; do
              [ -n "$f" ] || continue
              if GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null \
                GIT_CONFIG_NOSYSTEM=1 git -c core.fsmonitor=false \
                -C "$floor_worktree" cat-file -e "HEAD:$f" >/dev/null 2>&1; then
                if ! GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null \
                  GIT_CONFIG_NOSYSTEM=1 git -c core.hooksPath=/dev/null \
                  -c core.fsmonitor=false -C "$floor_worktree" checkout \
                  --quiet --force HEAD -- "$f" >/dev/null 2>&1 ||
                  ! GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null \
                  GIT_CONFIG_NOSYSTEM=1 git -c core.fsmonitor=false \
                  -c diff.external= -C "$floor_worktree" diff --no-ext-diff \
                  --no-textconv --quiet HEAD -- "$f"; then
                  floor_restore_ok=0
                  floor_restore_detail="could not restore tracked surface path: $f"
                  break
                fi
              else
                if ! GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null \
                  GIT_CONFIG_NOSYSTEM=1 git -c core.fsmonitor=false \
                  -C "$floor_worktree" clean -fdx -- "$f" >/dev/null 2>&1 ||
                  [ -e "$floor_worktree/$f" ] || [ -L "$floor_worktree/$f" ]; then
                  floor_restore_ok=0
                  floor_restore_detail="could not remove added surface path: $f"
                  break
                fi
              fi
            done <<EOF
$verification_surface
EOF
            if [ "$floor_restore_ok" = 1 ]; then
              prefix_index=0
              while [ "$prefix_index" -lt "$prefix_count" ]; do
                prefix_id_raw="$(prefix_field "$prefix_index" id)" || fail "cannot read batch floor task"
                prefix_verify_raw="$(prefix_field "$prefix_index" verify)" || fail "cannot read batch floor verifier"
                prefix_id="${prefix_id_raw%__OMS_PREFIX_END__}"
                prefix_verify="${prefix_verify_raw%__OMS_PREFIX_END__}"
                if prefix_out="$(cd "$floor_worktree" && OMS_TASK_ID="$prefix_id" OMS_PEER_VERIFY_TIMEOUT="${OMS_PEER_VERIFY_TIMEOUT:-20m}" run_verify_with_timeout bash -c "$prefix_verify" 2>&1)"; then
                  record "verify-prefix-floor-$prefix_id" "PASS" "$prefix_verify (restored from HEAD: $surface_detail)"
                else
                  prefix_exit=$?
                  timeout_note="$(OMS_PEER_VERIFY_TIMEOUT="${OMS_PEER_VERIFY_TIMEOUT:-20m}" ma_verify_timeout_note "$prefix_exit")"
                  record "verify-prefix-floor-$prefix_id" "FAIL" "$prefix_verify${timeout_note:+ — $timeout_note}"
                  echo "patch-admit: earlier task $prefix_id verifier failed with HEAD checks restored; candidate refused" >&2
                  floor_verify_out="$prefix_out"
                fi
                prefix_index=$((prefix_index + 1))
              done
              floor_verify_started="$(date +%s)"
              if floor_verify_out="$(cd "$floor_worktree" && OMS_PEER_VERIFY_TIMEOUT="${OMS_PEER_VERIFY_TIMEOUT:-20m}" run_verify_with_timeout bash -c "$floor_verify" 2>&1)"; then
                floor_verify_exit=0
                floor_verify_status=0
              else
                floor_verify_exit=$?
                floor_verify_status=1
              fi
              floor_verify_seconds=$(( $(date +%s) - floor_verify_started ))
              if [ "$floor_verify_status" -eq 0 ]; then
                record "verify-floor" "PASS" "$floor_verify (restored from HEAD: $surface_detail) — ${floor_verify_seconds}s"
              else
                floor_timeout_note="$(OMS_PEER_VERIFY_TIMEOUT="${OMS_PEER_VERIFY_TIMEOUT:-20m}" ma_verify_timeout_note "$floor_verify_exit")"
                if [ -n "$ALLOW_FLOOR_CHANGE_REASON" ] && [ "$VERIFY_EXIT" = 0 ] && [ "$VERIFICATION" = command ]; then
                  FLOOR_CHANGE_USED=true
                  record "verify-floor" "PASS" "owner override: base floor failed (exit $floor_verify_exit); reason: $ALLOW_FLOOR_CHANGE_REASON; $floor_verify (restored from HEAD: $surface_detail) — ${floor_verify_seconds}s${floor_timeout_note:+ — $floor_timeout_note}"
                else
                  floor_hint=""
                  [ "${OMS_HARNESS_CHILD:-0}" = 1 ] ||
                    floor_hint=" (intended contract change? owner override: --allow-floor-change REASON)"
                  record "verify-floor" "FAIL" "$floor_verify (restored from HEAD: $surface_detail)$floor_hint — ${floor_verify_seconds}s${floor_timeout_note:+ — $floor_timeout_note}"
                fi
              fi
            else
              record "verify-floor" "FAIL" "$floor_restore_detail"
            fi
          else
            record "verify-floor" "FAIL" "patch did not apply to the base-floor worktree"
          fi
        else
          record "verify-floor" "FAIL" "could not create the base-floor worktree"
        fi
      else
        record "verify-floor" "FAIL" "could not allocate the base-floor worktree"
      fi
    elif [ -n "$VERIFY" ]; then
      record "verify-floor" "SKIP" "verification surface unchanged"
    else
      record "verify-floor" "SKIP" "no verification command"
    fi
    else
      record "apply-worktree" "FAIL" "patch did not apply to the admission worktree (tree state differs from HEAD)"
    fi
  else
    record "worktree" "FAIL" "could not create admission worktree"
  fi
fi

# --- Security advice: informational only; it cannot alter admission ---------
security_scan="$(
  OMS_PATCH_ADMIT_LIB="$ROOT_LIB" OMS_PATCH_ADMIT_CHANGED="$changed_files" \
    OMS_PATCH_ADMIT_PATCH="$PATCH" python3 - 2>/dev/null <<'PY_SECURITY_ADVICE'
import json
import os
import sys
from pathlib import Path

sys.path.insert(0, os.environ["OMS_PATCH_ADMIT_LIB"])
from panel_review import _AUTO_REVIEW_FRONTIER_PATHS, _security_audit_paths

changed = [path for path in os.environ.get("OMS_PATCH_ADMIT_CHANGED", "").split("\n") if path]
patch = Path(os.environ["OMS_PATCH_ADMIT_PATCH"]).read_bytes()
paths = _security_audit_paths(changed, patch, _AUTO_REVIEW_FRONTIER_PATHS)
if paths:
    shown = ", ".join(json.dumps(path, ensure_ascii=True) for path in paths[:5])
    more = " (+%d more)" % (len(paths) - 5) if len(paths) > 5 else ""
    print("ADVISE\ttouches risk paths %s%s; run a security-auditor before landing" % (shown, more))
else:
    print("SKIP\tno risk paths")
PY_SECURITY_ADVICE
)" || security_scan=""
security_status="${security_scan%%$'\t'*}"
security_detail="${security_scan#*$'\t'}"
if [ "$security_status" != "ADVISE" ] && [ "$security_status" != "SKIP" ]; then
  security_status="ADVISE"
  security_detail="risk path scan unavailable; run a security-auditor before landing"
fi
record "security" "$security_status" "$security_detail"

if [ -n "$PLAN_TASK" ]; then
  current_prefix="$(python3 "$ROOT_LIB/landing-capture.py" batch-prefix "$reference_state" "$REPO" "$PLAN_TASK")" || current_prefix=""
  if [ "$current_prefix" != "$batch_prefix" ]; then
    record "batch-prefix" "FAIL" "batch prefix changed during admission"
  fi
fi

# REJECT if any gate failed (SKIP does not reject).
case "$ladder" in
  *"	FAIL	"*) verdict="REJECT" ;;
esac
# Parent-only hint, also seen through patch-land (it inherits stderr): the
# base verifier failed on the candidate and nothing else rejected.
if [ -n "$floor_hint" ] && [ "$(printf '%s' "$ladder" | grep -c "	FAIL	")" = 1 ]; then
  echo "patch-admit:$floor_hint" >&2
fi

# --- Report -----------------------------------------------------------------
ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
if [ -z "$REPORT" ]; then
  REPORT="$REPO/.oms/artifacts/admit/admit-$patch_sha-$(printf '%s' "$ts" | tr -c 'A-Za-z0-9' '-').md"
  agent_memory_ensure_oms_ignore_for_path "$REPO/.oms/artifacts/admit" 2>/dev/null || true
fi
mkdir -p "$(dirname "$REPORT")"
{
  printf '# Patch admission: %s\n\n' "$verdict"
  printf -- '- patch: %s\n' "$PATCH"
  printf -- '- patch_sha: %s\n' "$patch_sha"
  printf -- '- base: %s\n' "$base_sha"
  printf -- '- verification: %s\n' "$VERIFICATION"
  printf -- '- verify_sha256: %s\n' "$(printf "%s" "$VERIFY" | oms_sha256_stream)"
  if [ -n "$reuse_binding" ]; then
    printf -- '- admission_binding_sha256: %s\n' "$(printf "%s" "$reuse_binding" | python3 "$ROOT_LIB/landing-capture.py" binding-digest "$reference_state")"
    printf -- '- floor_verify_sha256: %s\n' "$(printf "%s" "${floor_verify:-}" | oms_sha256_stream)"
    printf -- '- verification_surface_sha256: %s\n' "$(printf "%s" "${verification_surface:-}" | oms_sha256_stream)"
  fi
  if [ -n "$ALLOW_FLOOR_CHANGE_REASON" ]; then
    printf -- '- floor_change_reason: %s\n' "$ALLOW_FLOOR_CHANGE_REASON"
    printf -- '- floor_change_used: %s\n' "$FLOOR_CHANGE_USED"
  fi
  if [ "$REGRESSION_SET" = 1 ]; then
    printf -- '- regression-proof: %s\n' "${regression_result:-inconclusive: patch did not apply}"
    printf -- '- regression-command: %s\n' "$REGRESSION_TEST"
    printf -- '- regression-base-digest: %s\n' "${regression_base_digest:-unavailable}"
    printf -- '- regression-candidate-digest: %s\n' "${regression_candidate_digest:-unavailable}"
    printf -- '- regression-base-exit: %s\n' "${regression_base_exit:-not-run}"
    printf -- '- regression-candidate-exit: %s\n' "${regression_candidate_exit:-not-run}"
  fi
  [ -z "$COVERS" ] || printf -- '- covers:%s\n' "$COVERS"
  printf -- '- checked: %s\n\n' "$ts"
  printf '## Ladder\n\n'
  printf '%s' "$ladder" | while IFS=$'\t' read -r gate status detail; do
    [ -n "$gate" ] || continue
    printf -- '- %s: %s — %s\n' "$gate" "$status" "$detail"
  done
  if [ -n "$changed_files" ]; then
    printf '\n## Changed files\n\n'
    printf '%s\n' "$changed_files" | while IFS= read -r f; do
      [ -n "$f" ] && printf -- '- %s\n' "$f"
    done
  fi
  if [ "$REGRESSION_SET" = 1 ]; then
    printf '\n## Regression base output (tail)\n\n```\n%s\n```\n' "${regression_base_out:-}"
    printf '\n## Regression candidate output (tail)\n\n```\n%s\n```\n' "${regression_candidate_out:-}"
  fi
  if [ -n "$verify_out" ]; then
    printf '\n## Verify output (tail)\n\n```\n%s\n```\n' "$(printf '%s' "$verify_out" | tail -n 40)"
  fi
  if [ "$FLOOR_CHANGE_USED" = true ]; then
    printf '\n## Base-floor verify output (owner override)\n\n```\n%s\n```\n' "${floor_verify_out:-(no output)}"
  elif [ -n "$floor_verify_out" ]; then
    printf '\n## Base-floor verify output (tail)\n\n```\n%s\n```\n' "$(printf '%s' "$floor_verify_out" | tail -n 40)"
  fi
} > "$REPORT"

# Index the admission so the audit trail survives `artifact-index.sh prune
# --files` (which removes unreferenced files under .oms/artifacts/).
append_admission_index() {
  if [ "$managed_input" = 1 ]; then
    # The ownership witness lives with shared state; its Git base still belongs
    # to the physical worktree where admission actually ran.
    OMS_ARTIFACT_INDEX="$reference_state/.oms/artifacts/index.jsonl" \
      OMS_INDEX_BASE_REPO="$REPO" ma_append_artifact_index "$reference_state" "$@"
  else
    ma_append_artifact_index "$REPO" "$@"
  fi
}
admit_exit=1
[ "$verdict" = "ADMIT" ] && admit_exit=0
admission_coverage() {
  OMS_PA_COVERS_IDS="$COVERS" OMS_PA_EXIT="$admit_exit" OMS_PA_SCOPE="$patch_sha" \
    OMS_PA_VERIFICATION="$VERIFICATION" OMS_PA_VERIFY_EXIT="$VERIFY_EXIT" python3 - <<'COVER_PY'
import json, os
ids = os.environ["OMS_PA_COVERS_IDS"].split()
status = "failed" if os.environ["OMS_PA_EXIT"] != "0" else "inconclusive"
if status != "failed" and os.environ["OMS_PA_VERIFICATION"] == "command" and os.environ["OMS_PA_VERIFY_EXIT"] == "0":
    status = "verified"
print(json.dumps({"covers": ids, "status": status, "scope_digest": os.environ["OMS_PA_SCOPE"]}))
COVER_PY
}
admit_covers="$(admission_coverage)" || fail "cannot compose admission evidence"
if ! OMS_INDEX_COVERS_JSON="$admit_covers" OMS_INDEX_OPERATION_ID="$reference_operation" \
  append_admission_index patch-admit "" "$admit_exit" "$REPORT" "$PATCH" "" "$VERIFY_EXIT"; then
  if [ -n "$COVERS${OMS_TASK_ID:-}" ] || [ "$managed_input" = 1 ]; then
    fail "admission receipt was not indexed; failing closed"
  fi
fi
if [ "$verdict" = "ADMIT" ]; then
  work_journal_observe "$REPO" patch-admit "$REPORT" \
    --source-id "admit:$patch_sha:$base_sha:$verdict" \
    --outcome "Patch admission passed" --outcome-status success \
    --verification-status "$([ "$VERIFICATION" = command ] && echo passed || echo skipped)"
else
  work_journal_observe "$REPO" patch-admit "$REPORT" \
    --source-id "admit:$patch_sha:$base_sha:$verdict" \
    --outcome "Patch admission rejected" --outcome-status failure \
    --verification-status failed
fi

if [ "$managed_input" = 1 ]; then
  oms_with_file_lock "$reference_state/.oms/artifacts/index.jsonl" python3 "$ROOT_LIB/landing-reference.py" \
    admission "$reference_state" "$PATCH" "$reference_operation" "$REPORT" "$admit_exit" ||
    fail "cannot bind native admission outcome to private capture"
fi
python3 "$ROOT_LIB/landing-reference.py" finish "$reference_state" "$reference_operation" 0 || fail "cannot finish reference operation"
reference_operation=""
oms_reference_stop "$reference_state" || fail "cannot stop reference monitor"
reference_scope_digest=""
if [ "$verdict" = ADMIT ] && [ -n "$reuse_binding" ]; then
  # Failure to prove unchanged inputs only disables reuse; it never fabricates
  # another ADMIT or changes the result of the checks that actually ran.
  python3 "$ROOT_LIB/landing-capture.py" bind-admission "$reference_state" "$PATCH" "$REPO" \
    "$reuse_binding" "$VERIFY" "${floor_verify:-}" "${verification_surface:-}" "${reuse_parameters[@]}" ||
    echo "warning: admission inputs changed; landing will verify again" >&2
fi
if [ "$verdict" = REJECT ] && [ "$intake_owned" = 1 ]; then
  OMS_LOCK_TIMEOUT=2 oms_with_file_lock "$reference_state/.oms/landings.jsonl" \
    python3 -c 'import runpy,sys; runpy.run_path(sys.argv[1])["cleanup_rejected"](sys.argv[2],sys.argv[3])' \
    "$ROOT_LIB/landing-capture.py" "$reference_state" "$intake_id" ||
    echo "warning: private rejected admission capture remains reserved" >&2
fi
echo "patch-admit: $verdict ($REPORT)" >&2
printf '%s\n' "$verdict"
[ "$verdict" = "ADMIT" ]
