#!/usr/bin/env bash
set -euo pipefail

# Land a committed worktree on the shared branch as one detached job. It runs
# the local gate, pushes without the pre-push hook, waits for CI, refreshes the
# install when the repo is the harness checkout itself, and writes a receipt;
# an agent starts it and reads `oms land status` instead of babysitting the gate.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/install-contract.sh
. "$ROOT/scripts/lib/install-contract.sh"
# shellcheck source=scripts/lib/work-journal.sh
. "$ROOT/scripts/lib/work-journal.sh"
# shellcheck source=scripts/lib/file-lock.sh
. "$ROOT/scripts/lib/file-lock.sh"
# shellcheck source=scripts/lib/test-growth.sh
. "$ROOT/scripts/lib/test-growth.sh"
# shellcheck source=scripts/lib/harness-residue.sh
. "$ROOT/scripts/lib/harness-residue.sh"
# shellcheck source=scripts/lib/land-receipts.sh
. "$ROOT/scripts/lib/land-receipts.sh"

usage() {
  cat <<'EOF'
Usage: land.sh [--repo PATH] [--remote NAME] [--target BRANCH] [--gate CMD]
               [--wait] [--no-update] [--ci-wait SECONDS]
               [--sibling-wait SECONDS] [--ignore-siblings] [--handoff-receipt-to PATH]
               [--sync-shared PATH]
       land.sh status [--repo PATH] [--json] [--active-exit]

Preconditions: a clean tracked tree, HEAD ahead of REMOTE/TARGET with the
remote tip as an ancestor (rebase first otherwise), and a gate command
(default: bash scripts/check.sh when the repo has one; the harness checkout
uses --affected --ci-parity against REMOTE/TARGET: a full selection runs lint
and the quick lane, without provider CLIs on PATH, plus a Bash 3.2 parse of
changed shell files, and leaves the full lanes to CI).
Stages, each recorded beside its gate log under
$XDG_STATE_HOME/oh-my-setting/land/<repo-slug>/<sha>-<request-digest>.json,
outside the repo:
  gate    the gate command, run in an independent clone of the verified
          commit (untracked files and checkout .oms stay out); a failure
          is recorded in the fail ledger
  push    git push --no-verify REMOTE VERIFIED_SHA:TARGET, only if HEAD and the tree
          are unchanged since the gate started
  ci      the GitHub run for the pushed commit, polled up to --ci-wait
          seconds (default 1500; 0 skips; needs gh)
  update  only after CI success plus full proof for the SHA: a full local
          gate (gate.scope=full) or a CI run whose jobs show the full
          selection (ci.scope=full; an affected-only or unreadable run
          does not count), the matching install checkout's update.sh; its
          target must still be the verified SHA. update.authority records
          which proof installed. Skipped CI never updates.
  sibling live autopilot runs (proposing, proposal-review, driving) in sibling
          worktrees are reported at intake and waited for before push, up to
          --sibling-wait seconds (default 1800; 0 checks once and blocks at
          once, it does not skip: --ignore-siblings skips)
Status probes the land lock without taking it and reports active true|false|
null (null: cannot tell). A receipt that says running proves nothing; active
does. Text status prints the lock observation explicitly. With --active-exit,
status exits 3 when active is confirmed, 0 when free is confirmed, and 4 when
the lock state is unknown.
With --handoff-receipt-to PATH, a passed gate/push/CI-success receipt and its
pushed range-boundary predecessor, when recorded, are imported byte-for-byte
into PATH's clone-local receipt namespace. PATH must have the same remote name and URL as
the pushed destination. Only its destination ref/objects are fetched; its
checkout is untouched. Skipped CI transfers nothing and the handoff fails.
When a push moves the destination, push.base records the observed pre-push tip
and fixes the landed range to push.base..SHA. Receipts without it infer the
boundary from predecessors.
Conflicting receipt bytes or a different inferred range boundary are refused;
identical bytes are idempotent. A failed handoff exits nonzero, preserving the
passed producer receipt for retry. Without this flag, clones remain isolated.
To retry only the handoff of an already-passed receipt, also pass --no-update.
This preserves receipt bytes and accepts a landed SHA still reachable from the
fetched target even if another main advanced it. Without --no-update, the normal
CI/install steps and exact-tip safety checks run before handoff.
With --sync-shared PATH, refresh PATH with sync-shared only after land passes
and any receipt handoff succeeds. PATH is required; it is independent of the
handoff path. Live calls defer sync (75); dirty, detached, ahead or diverged
checkouts refuse it. Sync never changes a passed land's exit status. Its result
is logged and recorded as sync.state in a companion sync/<request>.json receipt,
which status includes without changing the immutable landing/handoff bytes.
Retry the same command after deferred/refused sync to try the refresh again.
Without --wait the job detaches (setsid) and this prints the receipt path.
Retrying the same commit/destination/gate reuses successful push evidence and
resumes CI/install only. Concurrent jobs wait up to OMS_LOCK_TIMEOUT (default
300 seconds), then exit 75 if the lock remains held; waiter order is unspecified.
EOF
}

REPO="$PWD" REMOTE=origin TARGET=main GATE="" GATE_EXPLICIT=0 WAIT=0 UPDATE=1 CI_WAIT=1500
SIBLING_WAIT=1800 IGNORE_SIBLINGS=0 HANDOFF_TO="" SYNC_TO=""
MODE=start JSON=0 ACTIVE_EXIT=0 STAMP="" READY_FILE=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    status) MODE=status; shift ;;
    --run-job) MODE=job; STAMP="$2"; shift 2 ;;
    --ready-file) READY_FILE="$2"; shift 2 ;;
    --handoff-receipt-to)
      if [ "$#" -lt 2 ] || [ -z "$2" ]; then
        echo "error: --handoff-receipt-to requires PATH" >&2; exit 2
      fi
      HANDOFF_TO="$2"; shift 2 ;;
    --sync-shared)
      if [ "$#" -lt 2 ] || [ -z "$2" ] || [[ "$2" = --* ]]; then
        echo "error: --sync-shared requires PATH" >&2; exit 2
      fi
      SYNC_TO="$2"; shift 2 ;;
    --repo) REPO="$2"; shift 2 ;;
    --remote) REMOTE="$2"; shift 2 ;;
    --target) TARGET="$2"; shift 2 ;;
    --gate) GATE="$2"; GATE_EXPLICIT=1; shift 2 ;;
    --wait) WAIT=1; shift ;;
    --no-update) UPDATE=0; shift ;;
    --ci-wait) CI_WAIT="$2"; shift 2 ;;
    --sibling-wait) SIBLING_WAIT="$2"; shift 2 ;;
    --ignore-siblings) IGNORE_SIBLINGS=1; shift ;;
    --json) JSON=1; shift ;;
    --active-exit) ACTIVE_EXIT=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done
case "$CI_WAIT" in ''|*[!0-9]*) echo "error: --ci-wait must be seconds" >&2; exit 2 ;; esac
case "$SIBLING_WAIT" in ''|*[!0-9]*) echo "error: --sibling-wait must be seconds" >&2; exit 2 ;; esac
# A leading zero would make shell arithmetic read "08" as bad octal after push.
CI_WAIT=$((10#$CI_WAIT)) SIBLING_WAIT=$((10#$SIBLING_WAIT))
REPO="$(git -C "$REPO" rev-parse --show-toplevel 2>/dev/null)" ||
  { echo "error: --repo is not a git checkout: $REPO" >&2; exit 2; }
REPO="$(oms_strip_cr "$REPO")"
REPO="$(cd "$REPO" 2>/dev/null && pwd -P)" ||
  { echo "error: cannot resolve --repo path" >&2; exit 2; }

if [ -n "$HANDOFF_TO" ]; then
  if ! HANDOFF_TO="$(git -C "$HANDOFF_TO" rev-parse --show-toplevel 2>/dev/null | tr -d '\r')" ||
      ! HANDOFF_TO="$(cd "$HANDOFF_TO" && pwd -P)"; then
    echo "error: --handoff-receipt-to is not a git checkout" >&2; exit 2
  fi
fi

if [ -n "$SYNC_TO" ]; then
  if ! SYNC_TO="$(git -C "$SYNC_TO" rev-parse --show-toplevel 2>/dev/null | tr -d '\r')" ||
      ! SYNC_TO="$(cd "$SYNC_TO" && pwd -P)"; then
    echo "error: --sync-shared is not a git checkout" >&2; exit 2
  fi
fi

LAND_DIR="$(oms_land_state_dir "$REPO")" ||
  { echo "error: cannot resolve land state directory" >&2; exit 2; }
# Gate output and receipts share an XDG state directory. A gate purity check
# inventories .oms, so neither can live in the checkout.
LOG_DIR="$LAND_DIR"

# rset KEY=VALUE... merges fields into the receipt; dotted keys nest one
# level, integers stay integers.
rset() {
  python3 - "$RECEIPT" "$@" <<'PY'
import json, os, sys, tempfile
path, pairs = sys.argv[1], sys.argv[2:]
try:
    row = json.load(open(path, encoding="utf-8"))
except Exception:
    row = {"schema": 1}
if pairs and pairs[0] == "--reset":
    row, pairs = {"schema": 1}, pairs[1:]
for pair in pairs:
    key, _, value = pair.partition("=")
    if value.lstrip("-").isdigit():
        value = int(value)
    head, _, tail = key.partition(".")
    if tail:
        row.setdefault(head, {})[tail] = value
    else:
        row[head] = value
fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), prefix=".land.")
with os.fdopen(fd, "w", encoding="utf-8") as fh:
    json.dump(row, fh, ensure_ascii=False, sort_keys=True)
    fh.write("\n")
os.replace(tmp, path)
PY
}

now() { date -u +%Y-%m-%dT%H:%M:%SZ; }
clean_tree() { git -C "$REPO" diff --quiet && git -C "$REPO" diff --cached --quiet; }

SIBLING_LIVE="" SIBLING_FIRST_PATH="" SIBLING_FIRST_STAGE=""
SIBLING_INVALID_NOTED=""

note() {  # stderr, and the job log when detached (stderr is /dev/null there)
  echo "$1" >&2
  [ -z "${LOG:-}" ] || echo "$1" >> "$LOG"
}

note_invalid_sibling_receipt() {  # PATH
  local path="$1"

  if printf '%s\n' "$SIBLING_INVALID_NOTED" | grep -Fqx -- "$path"; then
    return 0
  fi
  note "note: ignoring invalid sibling autopilot receipt: $path"
  if [ -n "$SIBLING_INVALID_NOTED" ]; then
    SIBLING_INVALID_NOTED="$SIBLING_INVALID_NOTED
$path"
  else
    SIBLING_INVALID_NOTED="$path"
  fi
}

collect_sibling_worktree() {  # PATH BARE PRUNABLE REPORT
  local path="$1" bare="$2" prunable="$3" report="$4"
  local physical receipt metadata stage

  [ -n "$path" ] && [ "$bare" -eq 0 ] && [ "$prunable" -eq 0 ] || return 0
  physical="$(cd "$path" 2>/dev/null && pwd -P)" || return 0
  [ "$physical" != "$REPO" ] || return 0
  receipt="$physical/.oms/plan/autopilot-run.json"
  [ -e "$receipt" ] || [ -L "$receipt" ] || return 0
  metadata="$(python3 "$ROOT/scripts/lib/autopilot-receipt.py" metadata "$receipt" 2>/dev/null)" || {
    note_invalid_sibling_receipt "$physical"
    return 0
  }
  metadata="$(oms_strip_cr "$metadata")"
  stage="${metadata%%$'\t'*}"
  case "$stage" in
    proposing|proposal-review|driving) ;;
    *) return 0 ;;
  esac
  if [ -n "$SIBLING_LIVE" ]; then
    SIBLING_LIVE="$SIBLING_LIVE; $physical $stage"
  else
    SIBLING_LIVE="$physical $stage"
    SIBLING_FIRST_PATH="$physical"
    SIBLING_FIRST_STAGE="$stage"
  fi
  [ "$report" -eq 0 ] || note "land $SHORT: live sibling worktree $physical ($stage)"
}

collect_live_siblings() {  # REPORT (0 or 1)
  local report="$1" listing line path="" bare=0 prunable=0

  SIBLING_LIVE="" SIBLING_FIRST_PATH="" SIBLING_FIRST_STAGE=""
  listing="$(git -C "$REPO" worktree list --porcelain 2>/dev/null)" || {
    note "note: cannot enumerate sibling worktrees"
    return 1
  }
  while IFS= read -r line || [ -n "$line" ]; do
    line="$(oms_strip_cr "$line")"
    case "$line" in
      'worktree '*)
        collect_sibling_worktree "$path" "$bare" "$prunable" "$report"
        path="${line#worktree }" bare=0 prunable=0
        ;;
      bare) bare=1 ;;
      prunable*) prunable=1 ;;
      '')
        collect_sibling_worktree "$path" "$bare" "$prunable" "$report"
        path="" bare=0 prunable=0
        ;;
    esac
  done <<EOF
$listing
EOF
  collect_sibling_worktree "$path" "$bare" "$prunable" "$report"
}

wait_for_live_siblings() {
  local started deadline current remaining elapsed waited=0

  started="$(date +%s)"
  deadline="$(( started + SIBLING_WAIT ))"
  while :; do
    collect_live_siblings 0 || {  # fail closed: an unknown sibling state is live
      rset push.sibling_wait_seconds="$(( $(date +%s) - started ))"
      finish blocked "cannot enumerate sibling worktrees"
      return 1
    }
    elapsed="$(( $(date +%s) - started ))"
    if [ -z "$SIBLING_LIVE" ]; then
      [ "$waited" -eq 1 ] || elapsed=0
      rset push.sibling_wait_seconds="$elapsed"
      return 0
    fi
    current="$(date +%s)"
    if [ "$current" -ge "$deadline" ]; then
      [ "$waited" -eq 1 ] || elapsed=0
      rset push.sibling_wait_seconds="$elapsed"
      finish blocked "sibling worktree $SIBLING_FIRST_PATH still $SIBLING_FIRST_STAGE"
      return 1
    fi
    remaining="$(( deadline - current ))"
    [ "$remaining" -le 15 ] || remaining=15
    sleep "$remaining"
    waited=1
  done
}

repo_slug() {  # host/owner/name for ssh, https, and .git spellings alike
  printf '%s' "$1" | tr 'A-Z' 'a-z' |
    sed -E 's#^[a-z]+://##; s#^[^@/]+@##; s#^([^/:]+):#\1/#; s#\.git/?$##; s#/$##'
}

install_root() {  # the install checkout when REMOTE pushes to its origin; else nothing
  local receipt source theirs mine
  receipt="$(oms_install_receipt_path)"
  source="$(oms_install_receipt_field source_root "$receipt" 2>/dev/null)" || return 1
  [ -n "$source" ] && [ -f "$source/scripts/update.sh" ] || return 1
  theirs="$(git -C "$source" remote get-url origin 2>/dev/null)" || return 1
  mine="$(git -C "$REPO" remote get-url "$REMOTE" 2>/dev/null)" || return 1
  [ -n "$theirs" ] && [ "$(repo_slug "$theirs")" = "$(repo_slug "$mine")" ] && printf '%s\n' "$source"
}

finish() {  # finish STATE SUMMARY
  rset state="$1" summary="$2" finished_at="$(now)"
  work_journal_observe "$REPO" oms-run "$RECEIPT" --source-id "land:$STAMP" \
    --event-type phase_outcome --outcome "land $SHORT: $2" \
    --outcome-status "$([ "$1" = passed ] && echo success || echo failure)"
  echo "land $SHORT: $1: $2" >> "$LOG"
  if [ "$1" = passed ]; then
    after_land
  fi
}

after_land() {
  if [ -n "$HANDOFF_TO" ]; then
    handoff_receipt || return $?
  fi
  [ -z "$SYNC_TO" ] || sync_shared
}

sync_record() {
  # Post-land operations must not invalidate handed-off receipt digests.
  local RECEIPT="$LAND_DIR/sync/$STAMP.json"
  mkdir -p "$LAND_DIR/sync"
  rset "$@" sha="$SHA" request="$STAMP" sync.path="$SYNC_TO"
}

sync_shared() {
  local rc=0 state=passed reason='shared checkout refreshed' sync_log="$LOG.sync"
  sync_record --reset sync.state=running sync.started_at="$(now)"
  bash "$ROOT/scripts/sync-shared.sh" --repo "$SYNC_TO" --remote "$REMOTE" \
    --target "$TARGET" > "$sync_log" 2>&1 || rc=$?
  cat "$sync_log" >> "$LOG"
  if [ "$rc" -ne 0 ]; then
    state=refused
    [ "$rc" -ne 75 ] || state=deferred
    reason="$(tail -n 1 "$sync_log" | tr -d '\r')"
    reason="${reason#error: }"
    reason="${reason#deferred: }"
    [ -n "$reason" ] || reason="sync-shared exit $rc (see land log)"
  fi
  sync_record sync.state="$state" sync.reason="$reason" sync.rc="$rc" sync.finished_at="$(now)"
  note "land $SHORT: sync $state: $reason"
}

handoff_receipt() {
  local handoff_dir rc=0
  handoff_dir="$(oms_land_state_dir "$HANDOFF_TO")" || return 1
  if [ "$handoff_dir" = "$LAND_DIR" ]; then
    python3 "$ROOT/scripts/lib/land_receipts.py" "$REPO" "$HANDOFF_TO" "$RECEIPT" "$handoff_dir" >> "$LOG" 2>&1 || rc=$?
  else
    oms_with_file_lock "$handoff_dir/active" python3 "$ROOT/scripts/lib/land_receipts.py" \
      "$REPO" "$HANDOFF_TO" "$RECEIPT" "$handoff_dir" >> "$LOG" 2>&1 || rc=$?
  fi
  if [ "$rc" -ne 0 ]; then
    note "error: receipt handoff refused; passed producer receipt retained (see land log)"
  fi
  return "$rc"
}

receipt_value() {
  python3 - "$RECEIPT" "$1" <<'PY' | tr -d '\r'
import json, sys
try:
    value = json.load(open(sys.argv[1], encoding="utf-8"))
    for key in sys.argv[2].split("."):
        value = value[key]
    print(value)
except (OSError, ValueError, KeyError, TypeError):
    pass
PY
}

can_resume() {
  [ -f "$RECEIPT" ] &&
    [ "$(receipt_value sha)" = "$SHA" ] &&
    [ "$(receipt_value request)" = "$STAMP" ] &&
    [ "$(receipt_value gate.rc)" = 0 ] &&
    [ "$(receipt_value push.rc)" = 0 ]
}

can_handoff_only() {
  [ "$UPDATE" -eq 0 ] && [ -n "$HANDOFF_TO" ] && can_resume &&
    [ "$(receipt_value state)" = passed ] && [ "$(receipt_value ci.conclusion)" = success ]
}

request_stamp() {
  local destination
  destination="$(git -C "$REPO" remote get-url --push "$REMOTE")" || return 1
  # Hash instead of storing potentially credential-bearing URLs in receipts.
  printf '%s' "$destination" | python3 -c '
import hashlib, json, sys
print(sys.argv[1] + "-" + hashlib.sha256(json.dumps(
    [sys.stdin.read().strip(), *sys.argv[2:]], ensure_ascii=True
).encode()).hexdigest()[:24])' "$1" "$REMOTE" "$TARGET" "$GATE" | tr -d '\r'
}

# An independent clone keeps untracked state out of the exact-SHA gate without
# changing the source worktree registry observed by concurrent worker guards.
# A subshell owns cleanup traps without replacing the landing lock's traps.
run_gate() (
  local rc=0 parent tree
  parent="$(mktemp -d "$(oms_harness_tmp_base)/oh-my-setting-land.XXXXXX")" || return 1
  # Freeze the physical path so a TMPDIR symlink swapped mid-gate cannot
  # redirect cleanup to another directory.
  parent="$(cd "$parent" && pwd -P)" || return 1
  trap 'rm -rf "$parent"' EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
  tree="$parent/wt"
  # Local upload-pack copies objects without hardlinks or alternates. Fetch the
  # pinned SHA even if HEAD moved, and retain source remote-tracking refs used
  # by the default affected gate (clone's origin points at the local source).
  git -c core.fsmonitor=false clone --quiet --no-local --no-hardlinks --no-checkout \
    -- "$REPO" "$tree" >> "$LOG" 2>&1 || return 1
  git -C "$tree" fetch --quiet --no-tags -- "$REPO" "$SHA" \
    '+refs/remotes/*:refs/remotes/*' >> "$LOG" 2>&1 || return 1
  git -C "$tree" -c core.fsmonitor=false checkout --quiet --detach "$SHA" \
    >> "$LOG" 2>&1 || return 1
  (cd "$tree" && bash -c "$GATE") >> "$LOG" 2>&1 || rc=$?
  return "$rc"
)

run_job() {
  cd "$REPO"
  mkdir -p "$LAND_DIR"
  RECEIPT="$LAND_DIR/$STAMP.json" LOG="$LOG_DIR/$STAMP.log"
  SHA="$(git rev-parse HEAD)" SHORT="${SHA:0:7}"
  [ "$STAMP" = "$(request_stamp "$SHA")" ] || {
    note "error: HEAD or destination changed before job started"; return 1;
  }
  clean_tree || {
    note "error: tracked tree changed before job started"; return 1;
  }
  local resume=0 remote_sha failed_stages attempt marker
  if can_resume; then
    if can_handoff_only; then
      # The helper fetches the target ref and proves ancestry. Receipt-only
      # retries neither reinstall an old SHA nor rewrite imported bytes.
      after_land
      return $?
    fi
    remote_sha="$(git ls-remote --exit-code "$(git remote get-url --push "$REMOTE")" "refs/heads/$TARGET")" || return 1
    remote_sha="${remote_sha%%[[:space:]]*}"
    if [ "$remote_sha" != "$SHA" ] || ! clean_tree; then
      note "error: cannot resume: remote tip or tracked tree differs from verified commit"
      return 1
    fi
    resume=1
  fi
  [ "$resume" -eq 1 ] || rset --reset
  # Net test and code lines per push, kept to judge test growth by evidence.
  rset growth="$(oms_test_growth "$REMOTE/$TARGET" "$SHA")"
  rset pid="$$" state=running started_at="$(now)" sha="$SHA" gate.command="$GATE" \
    remote="$REMOTE" target="$TARGET" log="$LOG" request="$STAMP" resumed="$resume"
  attempt="$(($(grep -c '^land attempt: ' "$LOG" 2>/dev/null || true) + 1))"
  marker="land attempt: $attempt started_at=$(now) sha=$SHA"
  echo "$marker" >> "$LOG"
  # Detached startup is confirmed only after lock acquisition and the exact
  # SHA/tracked-tree admission checks above have written a running receipt.
  [ -z "$READY_FILE" ] || printf '0\n' > "$READY_FILE"
  local t0 rc=0
  if [ "$resume" -eq 0 ]; then
    rset gate.rc=pending push.rc=pending ci.conclusion=skipped update.rc=skipped
    if [ "$IGNORE_SIBLINGS" -eq 1 ]; then
      rset siblings.ignored=true
    else
      collect_live_siblings 1 || true
      [ -z "$SIBLING_LIVE" ] || rset siblings.live="$SIBLING_LIVE"
    fi
    t0="$(date +%s)"
    run_gate || rc=$?
    rset gate.rc="$rc" gate.seconds="$(( $(date +%s) - t0 ))"
    if [ "$rc" -ne 0 ]; then
      # Name the failing stages and smoke tests: which tests ever catch
      # anything is the input for retiring the ones that never do, and the
      # ledger is queryable where scattered gate logs are not.
      failed_stages="$(awk -v marker="$marker" 'index($0, marker) { current=1; next } current { print }' "$LOG" |
        sed -n 's/^check: \([^ ]*\) FAILED.*/\1/p' | sort -u | tr '\n' ' ')"
      "$ROOT/scripts/fail-ledger.sh" --repo "$REPO" record --kind verify --cmd "$GATE" \
        --exit "$rc" --summary "land: gate failed for $SHORT${failed_stages:+ in ${failed_stages% }} (see $LOG)" >/dev/null 2>&1 || true
      finish failed "gate exit $rc"; return 1
    fi
    # An affected selection lightens the push; installing needs a gate that proved the full check.
    if grep -qsE '^check: ok( \(parallel complete gate\)| \(affected tests; mode=full\))?$' "$LOG"; then
      rset gate.scope=full
    else
      rset gate.scope=partial
    fi
    if [ "$(git rev-parse HEAD)" != "$SHA" ] || ! clean_tree; then
      rset push.rc=-1
      finish failed "HEAD or the tree changed while the gate ran; nothing pushed"; return 1
    fi
    if [ "$IGNORE_SIBLINGS" -eq 0 ]; then
      wait_for_live_siblings || return 1
    fi
    if [ "$(git rev-parse HEAD)" != "$SHA" ] || ! clean_tree ||
        [ "$STAMP" != "$(request_stamp "$SHA")" ]; then
      rset push.rc=-1
      finish failed "HEAD, destination or tree changed before push; nothing pushed"; return 1
    fi
    remote_sha="$(git ls-remote --exit-code "$(git remote get-url --push "$REMOTE")" "refs/heads/$TARGET")" || {
      rset push.rc=-1
      finish failed "cannot read destination tip before push; nothing pushed"; return 1
    }
    remote_sha="${remote_sha%%[[:space:]]*}"
    if ! git merge-base --is-ancestor "$remote_sha" "$SHA"; then
      rset push.rc=-1
      finish failed "destination tip is not an ancestor of the verified commit; rebase first; nothing pushed"; return 1
    fi
    rc=0
    # The ancestry check keeps this fast-forward-only; the lease binds the
    # recorded base even if another clone pushes after the destination read.
    git push --no-verify --force-with-lease="refs/heads/$TARGET:$remote_sha" \
      "$REMOTE" "$SHA:refs/heads/$TARGET" >> "$LOG" 2>&1 || rc=$?
    if [ "$rc" -ne 0 ]; then
      rset push.rc="$rc"
      finish failed "push exit $rc"; return 1
    fi
    if [ "$remote_sha" != "$SHA" ]; then
      rset push.rc=0 push.base="$remote_sha"
    else
      # A reverify moved nothing: it proves the gate, never a landed range.
      rset push.rc=0 push.moved=0
    fi
  fi
  local conclusion=skipped run_id="" deadline remaining query_result
  if [ "$resume" -eq 1 ] && [ "$(receipt_value ci.conclusion)" = success ]; then
    conclusion=success
  elif [ "$CI_WAIT" -gt 0 ] &&
      ! git -C "$REPO" ls-tree -r --name-only "$SHA" -- .github/workflows | grep -Eq '\.ya?ml$'; then
    # No workflow can ever report on this commit; waiting only ends in timeout.
    rset ci.conclusion=skipped ci.reason=no-workflows
  elif [ "$CI_WAIT" -gt 0 ]; then
    conclusion=timeout
    rset ci.conclusion=pending
    deadline="$(( $(date +%s) + CI_WAIT ))"
    local queried=0
    # A budget that expires between two clock reads still owes one query.
    while [ "$queried" = 0 ] || [ "$(date +%s)" -lt "$deadline" ]; do
      local workflow=""
      [ ! -f "$REPO/.github/workflows/test.yml" ] || workflow=test.yml
      remaining="$(( deadline - $(date +%s) ))"
      if [ "$remaining" -le 0 ]; then
        [ "$queried" = 0 ] || break
        remaining=1
      fi
      queried=1
      [ "$remaining" -le 30 ] || remaining=30
      rc=0
      query_result="$(python3 "$ROOT/scripts/lib/ci-query.py" \
        "$(git remote get-url --push "$REMOTE")" "$SHA" "$remaining" "$TARGET" "$workflow" \
        2>> "$LOG")" || rc=$?
      rset ci.query_rc="$rc"
      if [ "$rc" -ne 0 ]; then
        rset ci.conclusion=query-error
        finish failed "pushed $SHORT but CI query exit $rc (see $LOG); retry to resume"
        return 1
      fi
      query_result="$(oms_strip_cr "$query_result")"
      read -r run_id conclusion <<< "$query_result"
      # GitHub's "skipped" means no job verified this commit; the bare word is
      # this script's own no-wait result, which must not be spoofed by a run.
      [ "$conclusion" != skipped ] || conclusion=run-skipped
      rset ci.run_id="$run_id" ci.conclusion="$conclusion"
      [ "$run_id" != 0 ] && [ "$conclusion" != pending ] && break
      conclusion=timeout
      remaining="$(( deadline - $(date +%s) ))"
      [ "$remaining" -gt 0 ] || break
      [ "$remaining" -le 30 ] || remaining=30
      sleep "$remaining"
    done
    rset ci.run_id="${run_id:-}" ci.conclusion="$conclusion"
  else
    rset ci.conclusion=skipped
  fi
  case "$conclusion" in
    success|skipped) ;;
    *) finish failed "pushed $SHORT but ci $conclusion"; return 1 ;;
  esac
  local install installed_tree authority="" ci_scope
  # A skipped update records why, so a passed land that left the install
  # behind is distinguishable from one that could not find it.
  if [ "$UPDATE" -ne 1 ]; then
    rset update.reason=no-update
  elif ! install="$(install_root)"; then
    rset update.reason=no-install-for-remote
  fi
  # Install needs full proof for this SHA: the local full gate, or a successful
  # CI run that executed the full selection. CI's selector already runs the
  # full lanes for exactly the ranges the local gate would fall back on.
  if [ "$UPDATE" -eq 1 ] && [ -n "${install:-}" ]; then
    if [ "$(receipt_value gate.scope)" = full ]; then
      authority=gate.scope=full
    elif [ "$conclusion" = success ]; then
      run_id="${run_id:-$(receipt_value ci.run_id)}"
      if [ "$resume" -eq 1 ] && [ "$(receipt_value ci.scope)" = full ]; then
        ci_scope=full
      else
        rc=0
        ci_scope="$(python3 "$ROOT/scripts/lib/ci-query.py" scope \
          "$(git remote get-url --push "$REMOTE")" "${run_id:-0}" "$SHA" 30 2>> "$LOG")" || rc=$?
        ci_scope="$(oms_strip_cr "$ci_scope")"
        rset ci.scope_query_rc="$rc"
        if [ "$rc" -ne 0 ]; then
          rset ci.scope=unknown update.reason=ci-scope-unknown
          finish blocked "pushed $SHORT; ci success but its scope query exit $rc (see $LOG); install needs full proof; retry to resume"
          return 1
        fi
      fi
      rset ci.scope="$ci_scope"
      [ "$ci_scope" != full ] || authority=ci.scope=full
    fi
    if [ -z "$authority" ]; then
      rset update.reason=partial-gate
      finish passed "pushed $SHORT; ci $conclusion; install needs a full gate or a full-selection CI run: oms land --gate 'bash scripts/check.sh --parallel'"
      return $?
    fi
  fi
  if [ "$UPDATE" -eq 1 ] && [ -n "${install:-}" ]; then
    if [ "$conclusion" != success ]; then
      finish blocked "pushed $SHORT; install update requires successful CI (ci $conclusion)"
      return 1
    fi
    if [ "$resume" -eq 1 ] && [ "$(receipt_value update.rc)" = 0 ] &&
        [ "$(oms_strip_cr "$(receipt_value update.root)")" = "$install" ] &&
        [ "$(git -C "$install" rev-parse HEAD 2>/dev/null)" = "$SHA" ] &&
        installed_tree="$(git -C "$install" status --porcelain)" && [ -z "$installed_tree" ]; then
      finish passed "already pushed and installed $SHORT; ci $conclusion"
      return $?
    fi
    rc=0
    OH_MY_SETTING_UPDATE_EXPECTED_TARGET="$SHA" "$install/scripts/update.sh" >> "$LOG" 2>&1 || rc=$?
    rset update.rc="$rc" update.expected_sha="$SHA" update.root="$install" update.authority="$authority"
    if [ "$rc" -ne 0 ]; then
      finish failed "pushed $SHORT but install update exit $rc; ci $conclusion"
      return 1
    fi
    # A resumed install must not keep the reason an earlier attempt stopped for.
    rset update.reason=installed
  fi
  finish passed "pushed $SHORT to $REMOTE/$TARGET; ci $conclusion"
}

land_active() {  # true | false | null, from a read-only probe of the land lock
  case "$(oms_file_lock_probe "$LAND_DIR/active" 2>/dev/null)" in
    held) echo true ;;
    free) echo false ;;
    *) echo null ;;
  esac
}

show_status() {  # show_status [RECEIPT]; default: this HEAD's newest, else the newest
  local newest="${1:-}" active rc=0 head=""
  active="$(land_active)"
  if [ "$ACTIVE_EXIT" -eq 1 ]; then
    case "$active" in true) rc=3 ;; false) rc=0 ;; *) rc=4 ;; esac
  fi
  # Several mains share one land directory: this worktree's own HEAD is the
  # landing its caller means, and the newest receipt may be another main's.
  if [ -z "$newest" ] && head="$(git -C "$REPO" rev-parse -q --verify HEAD 2>/dev/null)"; then
    head="${head//$'\r'/}"
    newest="$(ls -t "$LAND_DIR/$head"-*.json 2>/dev/null | head -n 1)" || true
  fi
  [ -n "$newest" ] || newest="$(ls -t "$LAND_DIR"/*.json 2>/dev/null | head -n 1)" || true
  if [ -z "$newest" ]; then
    if [ "$ACTIVE_EXIT" -eq 1 ]; then
      if [ "$JSON" -eq 1 ]; then
        printf '{"active":%s}\n' "$active"
      else
        printf 'land lock: %s (no landing recorded)\n' "$active"
      fi
      return "$rc"
    fi
    echo "no landing recorded under $LAND_DIR"
    return 1
  fi
  python3 - "$newest" "$active" "$JSON" "${head:-}" <<'PY' || return $?
import calendar, json, sys, time
from pathlib import Path
r = json.load(open(sys.argv[1], encoding="utf-8"))
sync_path = Path(sys.argv[1]).parent / "sync" / Path(sys.argv[1]).name
try:
    sync = json.loads(sync_path.read_text(encoding="utf-8"))
    if (isinstance(sync, dict) and sync.get("sha") == r.get("sha") and
            sync.get("request") == r.get("request") and isinstance(sync.get("sync"), dict)):
        r["sync"] = sync["sync"]
except (OSError, ValueError):
    pass
active = {"true": True, "false": False}.get(sys.argv[2])
g, p, u, c = r.get("gate", {}), r.get("push", {}), r.get("update", {}), r.get("ci", {})
if sys.argv[3] == "1":
    r["active"] = active
    if active:
        r["step"] = ("gate" if g.get("rc") == "pending" else "push" if p.get("rc") == "pending"
                     else "ci" if c.get("conclusion") == "pending"
                     else "sync" if r.get("sync", {}).get("state") == "running" else "update")
        try:
            r["minutes"] = max(0, int((time.time() - calendar.timegm(time.strptime(
                r["started_at"], "%Y-%m-%dT%H:%M:%SZ"))) // 60))
        except (KeyError, ValueError, TypeError):
            pass
    print(json.dumps(r, ensure_ascii=False, sort_keys=True))
    sys.exit(0)
state = r.get("state")
if state == "running" and active is True:
    state = "running (lock held)"
elif state == "running" and active is False:
    state = "receipt says running, but no land holds the lock (stale receipt)"
elif state == "running" and active is None:
    state = "receipt says running, but lock activity is unknown"
print("land %s: %s" % (str(r.get("sha", ""))[:7], state))
if sys.argv[4] and r.get("sha") != sys.argv[4]:
    print("  note: newest land in this repository, not this worktree's HEAD %s" % sys.argv[4][:7])
print("  active: %s" % ("unknown" if active is None else str(active).lower()))
if u.get("rc") == "skipped":
    print("  install: skipped (%s)" % u.get("reason", "reason not recorded"))
print("  gate: %s (%ss)  push: %s  update: %s  ci: %s%s" % (
    "ok" if g.get("rc") == 0 else g.get("rc", "-"), g.get("seconds", "-"),
    "ok" if p.get("rc") == 0 else p.get("rc", "-"),
    "ok" if u.get("rc") == 0 else "%s%s" % (u.get("rc", "-"), " (%s)" % u["reason"] if u.get("reason") else ""),
    c.get("conclusion", "-"), " (run %s)" % c["run_id"] if c.get("run_id") else ""))
if u.get("authority") or c.get("scope"):
    print("  install proof: %s  ci scope: %s" % (u.get("authority", "-"), c.get("scope", "-")))
if r.get("summary"):
    print("  %s" % r["summary"])
if r.get("sync"):
    print("  sync %s: %s" % (r["sync"].get("state", "unknown"), r["sync"].get("reason", "in progress")))
s = r.get("siblings", {})
if s.get("ignored"):
    print("  siblings: ignored")
elif s.get("live"):
    print("  siblings: %s (waited %ss)" % (s["live"], p.get("sibling_wait_seconds", "-")))
print("  log: %s" % r.get("log", ""))
PY
  return "$rc"
}

land_ready_exit() {
  local rc="$?" ready_state=""

  [ -n "$READY_FILE" ] && [ -f "$READY_FILE" ] || return 0
  IFS= read -r ready_state < "$READY_FILE" || true
  [ "$ready_state" = waiting ] || return 0
  printf '%s\n' "$rc" > "$READY_FILE" 2>/dev/null || true
}

case "$MODE" in
  status) rc=0; show_status || rc=$?; exit "$rc" ;;
  job)
    if [ -n "$READY_FILE" ]; then
      printf 'waiting\n' > "$READY_FILE"
      # A detached launcher can distinguish lock waiting from a failed start.
      # The EXIT fallback covers signals before run_job writes its admission result.
      trap land_ready_exit EXIT
    fi
    rc=0
    oms_with_file_lock "$LAND_DIR/active" run_job || rc=$?
    if [ -n "$READY_FILE" ] && [ -f "$READY_FILE" ]; then
      ready_state=""
      IFS= read -r ready_state < "$READY_FILE" || true
    else
      ready_state=""
    fi
    if [ "$ready_state" = waiting ]; then
      printf '%s\n' "$rc" > "$READY_FILE"
    fi
    [ "$rc" -ne 75 ] || echo "land already active for this repository; use land status" >&2
    exit "$rc"
    ;;
esac

clean_tree || { echo "error: commit or stash tracked changes first" >&2; exit 2; }
if [ -z "$GATE" ]; then
  [ -f "$REPO/scripts/check.sh" ] || { echo "error: no scripts/check.sh here; pass --gate CMD" >&2; exit 2; }
  GATE="bash scripts/check.sh"
  if [ -f "$REPO/scripts/lib/check-parallel.sh" ] || install_root >/dev/null; then
    # Quote the destination as shell data: remote and branch names can contain
    # shell metacharacters even though Git accepts them.
    printf -v gate_destination '%q' "$REMOTE/$TARGET"
    GATE="$GATE --affected --ci-parity --changed-from $gate_destination --changed-to HEAD"
  fi
fi
# FETCH_HEAD lives in .git/worktrees/<id> for linked callers; only the remote
# tracking refs are needed here, so leave that protected registry untouched.
git -C "$REPO" fetch -q --no-write-fetch-head "$REMOTE" || { echo "error: fetch from $REMOTE failed" >&2; exit 2; }
SHA="$(git -C "$REPO" rev-parse HEAD)"
# Retries/worktrees share a receipt; different destinations/gates never share proof.
STAMP="$(request_stamp "$SHA")"
RECEIPT="$LAND_DIR/$STAMP.json"
# Only a receipt-only retry can use an ancestor of the destination tip. Its
# helper fetches and validates the target before importing any receipt bytes.
if ! can_handoff_only && ! git -C "$REPO" merge-base --is-ancestor "$REMOTE/$TARGET" "$SHA"; then
  echo "error: $REMOTE/$TARGET is not an ancestor of HEAD; rebase first" >&2; exit 2
fi
echo "land: this push changes $(oms_test_growth "$REMOTE/$TARGET" "$SHA" "$REPO")"
if [ "$(git -C "$REPO" rev-parse HEAD)" = "$(git -C "$REPO" rev-parse "$REMOTE/$TARGET")" ]; then
  # An explicit gate may still verify and install the already-pushed SHA.
  if ! can_resume && [ "$GATE_EXPLICIT" -eq 0 ]; then
    echo "nothing to land: HEAD is already $REMOTE/$TARGET; no matching push receipt to resume"; exit 0
  fi
fi
mkdir -p "$LAND_DIR"
job=("$ROOT/scripts/land.sh" --run-job "$STAMP" --repo "$REPO" --remote "$REMOTE" \
  --target "$TARGET" --gate "$GATE" --ci-wait "$CI_WAIT" --sibling-wait "$SIBLING_WAIT")
[ -z "$HANDOFF_TO" ] || job+=(--handoff-receipt-to "$HANDOFF_TO")
[ -z "$SYNC_TO" ] || job+=(--sync-shared "$SYNC_TO")
[ "$UPDATE" -eq 1 ] || job+=(--no-update)
[ "$IGNORE_SIBLINGS" -eq 0 ] || job+=(--ignore-siblings)
if [ "$WAIT" -eq 1 ]; then
  rc=0
  bash "${job[@]}" || rc=$?
  [ ! -f "$LAND_DIR/$STAMP.json" ] || JSON=0 show_status "$LAND_DIR/$STAMP.json"
  exit "$rc"
fi
# Job control on for the launch: a plain `&` from a script leaves SIGINT and
# SIGQUIT ignored in the child, and the gate's own interrupt tests then fail.
READY_FILE="$(mktemp "$LAND_DIR/.ready.XXXXXX")"
trap 'rm -f "$READY_FILE"' EXIT
set -m
setsid bash "${job[@]}" --ready-file "$READY_FILE" < /dev/null > /dev/null 2>&1 &
set +m
ready_timeout="$(oms_file_lock_timeout)"
ready_limit="$((10#$ready_timeout + 5))"
ready_state=""
waiting_reported=0
for ((ready_attempt=0; ready_attempt<ready_limit; ready_attempt++)); do
  if [ -s "$READY_FILE" ]; then
    IFS= read -r ready_state < "$READY_FILE" || ready_state=""
    case "$ready_state" in
      waiting)
        if [ "$waiting_reported" -eq 0 ]; then
          echo "land: waiting for active landing lock (timeout ${ready_timeout}s)" >&2
          waiting_reported=1
        fi
        ;;
      0) break ;;
      ''|*[!0-9]*) ready_state="" ;;
      *) break ;;
    esac
  fi
  sleep 1
done
if [ -s "$READY_FILE" ]; then
  IFS= read -r ready_state < "$READY_FILE" || ready_state=""
fi
if [ "$ready_state" != 0 ]; then
  case "$ready_state" in
    ''|*[!0-9]*)
      echo "error: landing start not confirmed; inspect land status before retrying" >&2
      exit 1
      ;;
    *)
      if [ "$ready_state" = 75 ]; then
        echo "land not started (exit 75); another landing may be active; use land status" >&2
      else
        echo "land not started (exit $ready_state); use land status" >&2
      fi
      exit "$ready_state"
      ;;
  esac
fi
echo "landing $(git -C "$REPO" rev-parse --short HEAD) in the background"
echo "receipt: $LAND_DIR/$STAMP.json"
echo "status: oms land status --repo $REPO"
