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
               [--wait] [--no-update] [--ci-wait SECONDS] [--ci-full]
               [--sibling-wait SECONDS] [--ignore-siblings] [--handoff-receipt-to PATH]
               [--sync-shared PATH]
       land.sh status [--repo PATH] [--json] [--active-exit]
       land.sh notes [--repo PATH] --from REV --to REV [--output NEW_FILE]
       land.sh blame-session [--repo PATH] [--json] [--file PATH] [--export-notes] COMMIT_OR_RANGE
       land.sh conflicts [--repo PATH] [--base REV] [--max N] [--json] [BRANCH...]

blame-session prints, per commit, the attribution row recorded in
.oms/attribution.jsonl (ai | human | mixed | unknown) or `unrecorded`, then a
summary line. See docs/COMPONENTS.md (Delegating writes).

conflicts previews, read-only, which pending local oms/* branches (or the named
branches) would conflict with the base and with each other, using
`git merge-tree --write-tree` (git 2.38+; older git prints a "too old" line).
It prints `clean` or `conflicts in FILES` per pair and a landing order that is
only a labelled suggestion. At most --max branches (default 12, cap 24) are
checked and the limit is printed. Repository config that would run a program
makes it print "not checked" instead of running it.

notes prints a release-notes skeleton for REV..REV: first-parent commit
subjects grouped by Conventional Commit type, with the commits this checkout's
land receipts recorded marked. It is read-only apart from --output, which
creates a new file and never replaces one; no tag, release or commit is made.

Preconditions: a clean tracked tree, HEAD ahead of REMOTE/TARGET with the
remote tip as an ancestor (rebase first otherwise), and a gate command
(default: bash scripts/check.sh when the repo has one; the harness checkout
uses --affected --ci-parity against REMOTE/TARGET: a full selection runs lint
and the quick lane, without provider CLIs on PATH, and leaves the full lanes to CI).
Stages, each recorded beside its gate log under
$XDG_STATE_HOME/oh-my-setting/land/<repo-slug>/<sha>-<request-digest>.json,
outside the repo:
  gate    the gate command, run in an independent clone of the verified
          commit (untracked files and checkout .oms stay out); a failure
          is recorded in the fail ledger
  push    git push --no-verify REMOTE VERIFIED_SHA:TARGET, only if HEAD and the tree
          are unchanged since the gate started
  ci      the GitHub run for the pushed commit, polled up to --ci-wait
          seconds (default 2700; 0 skips; gh or public OMS API fallback)
  update  only after CI success plus full proof for the SHA, the matching
          install checkout's update.sh; its target must still be the
          verified SHA. update.authority records which proof installed.
          Skipped CI never updates. Full proof is, by default, a successful
          CI run for exactly this SHA on TARGET whose jobs show the full
          selection (ci.scope=full): the push run when it was full, else a
          manually dispatched run (test.yml runs the full selection for
          every dispatch). When the push run was affected-only land
          finishes passed, installs nothing and prints the command to
          request that run (gh workflow run test.yml --ref TARGET); then
          rerun the same land command with --wait to check for it and wait
          within --ci-wait if pending. A pending run or requested handoff
          blocks until proof is complete. --ci-full lets land dispatch it once,
          recorded as ci.dispatch, and wait for it; without the flag land
          never dispatches. A run for another SHA, an unsuccessful or
          cancelled run, or unreadable scope never counts. The option is
          a full local gate (--gate 'bash scripts/check.sh --parallel',
          gate.scope=full), which still needs successful exact-SHA CI.
  sibling live autopilot runs (proposing, proposal-review, driving) in sibling
          worktrees are reported at intake and waited for before push, up to
          --sibling-wait seconds (default 1800; 0 checks once and blocks at
          once, it does not skip: --ignore-siblings skips); with --wait the same
          budget also retries an install deferred by live workers (update.sh
          exit 76) every 15s, counted from the first deferral
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
Live workers defer installation (update exit 76); retry the same command.
With --no-update, a receipt with successful gate/push but unsuccessful CI may
re-query CI for its exact SHA while that SHA remains reachable from the fetched
target; this never reruns the gate or push and never installs.
EOF
}

# Read-only: a release-notes skeleton from one range's commit subjects and this
# checkout's land receipts. It starts no job, takes no lock and pushes nothing.
if [ "${1:-}" = notes ]; then
  shift
  exec python3 "$ROOT/scripts/lib/land_notes.py" "$@"
fi

# Read-only: pairwise merge-conflict preview among pending oms/* branches. It
# starts no job, takes no lock and writes no ref, index, worktree or object.
if [ "${1:-}" = conflicts ]; then
  shift
  exec python3 -I -B "$ROOT/scripts/lib/pending_conflicts.py" "$@"
fi

# Read-only: which recorded attempt produced each commit of a range.
if [ "${1:-}" = blame-session ]; then
  shift
  blame_repo="$PWD" blame_prev=""
  for blame_arg in "$@"; do
    [ "$blame_prev" != --repo ] || blame_repo="$blame_arg"
    blame_prev="$blame_arg"
  done
  blame_state="$(oms_state_root "$blame_repo" 2>/dev/null)" || blame_state="$blame_repo"
  exec python3 "$ROOT/scripts/lib/attribution.py" blame --state-repo "$blame_state" "$@"
fi

REPO="$PWD" REMOTE=origin TARGET=main GATE="" GATE_EXPLICIT=0 WAIT=0 UPDATE=1 CI_WAIT=2700
SIBLING_WAIT=1800 IGNORE_SIBLINGS=0 HANDOFF_TO="" SYNC_TO="" CI_FULL=0
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
    --ci-full) CI_FULL=1; shift ;;
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
if [ "$CI_FULL" -eq 1 ] && { [ "$CI_WAIT" -eq 0 ] || [ "$UPDATE" -eq 0 ]; }; then
  echo "error: --ci-full needs --ci-wait above 0 and an install (no --no-update)" >&2; exit 2
fi
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

host_load() {
  python3 -c '
import os
try:
    load = "%.2f" % os.getloadavg()[0]
except (AttributeError, OSError):
    load = ""
try:
    cpus = os.cpu_count() or ""
except (AttributeError, OSError):
    cpus = ""
print("%s:%s" % (load, cpus))' 2>/dev/null | tr -d '\r'
}
clean_tree() { local s; s="$(git --no-optional-locks -C "$REPO" status --porcelain --untracked-files=no)" && [ -z "$s" ]; }

SIBLING_LIVE="" SIBLING_FIRST_PATH="" SIBLING_FIRST_STAGE=""
SIBLING_INVALID_NOTED=""

note() {  # stderr, and the job log when detached (stderr is /dev/null there)
  echo "$1" >&2
  [ -z "${LOG:-}" ] || echo "$1" >> "$LOG"
}

note_invalid_sibling_receipt() {  # PATH
  local path="$1"

  if printf '%s\n' "$SIBLING_INVALID_NOTED" | grep -Fx -- "$path" > /dev/null; then
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

record_live_sibling() {  # PATH STAGE REPORT
  local physical="$1" stage="$2" report="$3"

  if [ -n "$SIBLING_LIVE" ]; then
    SIBLING_LIVE="$SIBLING_LIVE; $physical $stage"
  else
    SIBLING_LIVE="$physical $stage"
    SIBLING_FIRST_PATH="$physical"
    SIBLING_FIRST_STAGE="$stage"
  fi
  [ "$report" -eq 0 ] || note "land $SHORT: live sibling worktree $physical ($stage)"
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
  record_live_sibling "$physical" "$stage" "$report"
}

# A campaign executing in a scratch keeps its receipt in the canonical state
# root, bound to the scratch: no sibling checkout holds a receipt of its own.
collect_state_root_campaign() {  # REPORT
  local report="$1" root receipt metadata stage bound physical

  root="$(oms_state_root "$REPO" 2>/dev/null)" || return 0
  root="$(oms_strip_cr "$root")"
  root="$(cd "$root" 2>/dev/null && pwd -P)" || return 0
  receipt="$root/.oms/plan/autopilot-run.json"
  [ -e "$receipt" ] || [ -L "$receipt" ] || return 0
  metadata="$(python3 "$ROOT/scripts/lib/autopilot-receipt.py" metadata "$receipt" 2>/dev/null)" || {
    note_invalid_sibling_receipt "$root"
    return 0
  }
  metadata="$(oms_strip_cr "$metadata")"
  stage="${metadata%%$'\t'*}"
  case "$stage" in
    proposing|proposal-review|driving) ;;
    *) return 0 ;;
  esac
  bound="${metadata#*$'\t'*$'\t'*$'\t'}"
  [ -n "$bound" ] || return 0
  # An unresolvable receipt repo cannot be proven to be this checkout.
  physical="$bound"
  # An empty path never reaches cd, which would succeed in the cwd (this checkout).
  if [ -n "$physical" ]; then
    physical="$(cd "$physical" 2>/dev/null && pwd -P)" || physical=""
  fi
  [ -n "$physical" ] || { record_live_sibling "$bound" "$stage" "$report"; return 0; }
  [ "$physical" != "$REPO" ] || return 0
  record_live_sibling "$physical" "$stage" "$report"
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
  collect_state_root_campaign "$report"
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
  # A linked sync namespace could alias the landing receipt; refuse rather than follow it.
  if [ -L "$LAND_DIR/sync" ] || { [ -e "$LAND_DIR/sync" ] && [ ! -d "$LAND_DIR/sync" ]; } || [ -L "$RECEIPT" ]; then
    echo "error: land sync receipt namespace is not a plain directory" >> "$LOG"; return 1
  fi
  mkdir -p "$LAND_DIR/sync"
  rset "$@" sha="$SHA" request="$STAMP" sync.path="$SYNC_TO"
}

sync_shared() {
  local rc=0 state=passed reason='shared checkout refreshed' sync_log="$LOG.sync"
  if ! sync_record --reset sync.state=running sync.started_at="$(now)"; then
    note "land $SHORT: sync refused: sync receipt namespace is not a plain directory"
    return 0
  fi
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

# Best effort: tie the pushed commits to recorded worker-patch landings. A
# ledger problem warns in the log and never changes the landing outcome.
record_attribution() {  # BASE_SHA
  local state out
  state="$(oms_state_root "$REPO" 2>/dev/null)" || return 0
  out="$(python3 "$ROOT/scripts/lib/attribution.py" commits --repo "$REPO" --state-repo "$state" \
    --range "$1..$SHA" --receipt "$(basename "$RECEIPT" .json)" 2>&1)" || true
  [ -z "$out" ] || note "$out"
  return 0
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

# CI success can be finalized from a running receipt if the prior retry stopped before finish.
can_resume_ci_only() {
  [ "$UPDATE" -eq 0 ] && can_resume &&
    { [ "$(receipt_value ci.conclusion)" != success ] || [ "$(receipt_value state)" = running ]; } &&
    git -C "$REPO" merge-base --is-ancestor "$SHA" "$REMOTE/$TARGET"
}

land_retry_command() {  # the command that resumes this landing from its receipt
  local retry_args=("$ROOT/scripts/land.sh" --repo "$REPO" --remote "$REMOTE" --target "$TARGET" --gate "$GATE" --wait --ci-wait "$CI_WAIT" --sibling-wait "$SIBLING_WAIT")
  [ -z "$HANDOFF_TO" ] || retry_args+=(--handoff-receipt-to "$HANDOFF_TO")
  [ -z "$SYNC_TO" ] || retry_args+=(--sync-shared "$SYNC_TO")
  [ "$IGNORE_SIBLINGS" -eq 0 ] || retry_args+=(--ignore-siblings)
  [ "$CI_FULL" -eq 0 ] || retry_args+=(--ci-full)
  printf '%q ' "${retry_args[@]}"
}

# Full proof from CI when the push run was affected-only: a manually dispatched
# test.yml run (always the full selection) for exactly SHA on TARGET. Sets
# ROUTE_SCOPE=full only for a completed successful run whose scope query says
# full; otherwise ROUTE_NOTE says why not. Dispatches only with --ci-full, once
# per receipt, and only while TARGET is still SHA. Returns 1 when it cannot
# tell (query or dispatch error); that never counts as proof.
ci_full_route() {
  local destination slug deadline remaining result rid concl rc=0 tip
  ROUTE_SCOPE="" ROUTE_NOTE="" ROUTE_PENDING=0
  [ -f "$REPO/.github/workflows/test.yml" ] || { ROUTE_NOTE="no test.yml workflow"; return 0; }
  destination="$(git remote get-url --push "$REMOTE")"
  deadline="$(( $(date +%s) + CI_WAIT ))"
  while :; do
    remaining="$(( deadline - $(date +%s) ))"
    [ "$remaining" -gt 0 ] || remaining=0
    rc=0
    result="$(python3 "$ROOT/scripts/lib/ci-query.py" dispatch "$destination" "$SHA" "$(( remaining > 30 ? 30 : remaining ))" \
      "$TARGET" test.yml 2>> "$LOG")" || rc=$?
    rset ci.full_query_rc="$rc"
    if [ "$rc" -ne 0 ]; then
      ROUTE_NOTE="full-run query exit $rc (see $LOG)"
      return 1
    fi
    result="$(oms_strip_cr "$result")"
    read -r rid concl <<< "$result"
    if [ "$rid" != 0 ]; then
      if [ "$concl" != pending ]; then break; fi
      ROUTE_PENDING=1
    fi
    if [ "$rid" = 0 ] && [ "$(receipt_value ci.dispatch)" != requested ] && [ "$CI_FULL" -eq 0 ]; then
      ROUTE_NOTE="no dispatched full run for this SHA"
      return 0
    fi
    if [ "$rid" = 0 ] && [ "$(receipt_value ci.dispatch)" != requested ] && [ "$CI_FULL" -eq 1 ]; then
      while :; do
        remaining="$(( deadline - $(date +%s) ))"
        if [ "$remaining" -le 0 ]; then
          ROUTE_NOTE="remote tip lookup failed within --ci-wait (see $LOG)"
          return 1
        fi
        [ "$remaining" -le 30 ] || remaining=30
        rc=0
        tip="$(python3 "$ROOT/scripts/lib/ci-query.py" tip "$destination" "$TARGET" "$remaining" 2>> "$LOG")" || rc=$?
        [ "$rc" -ne 0 ] || break
        remaining="$(( deadline - $(date +%s) ))"
        if [ "$remaining" -le 0 ]; then
          ROUTE_NOTE="remote tip lookup failed within --ci-wait (see $LOG)"
          return 1
        fi
        [ "$remaining" -le 2 ] || remaining=2
        sleep "$remaining"
      done
      if [ "$(oms_strip_cr "$tip")" != "$SHA" ]; then
        ROUTE_NOTE="$REMOTE/$TARGET moved past this SHA; a dispatch would not run it"
        return 1
      fi
      slug="$(python3 "$ROOT/scripts/lib/ci-query.py" query-repo "$destination")" || { ROUTE_NOTE="invalid repository destination"; return 1; }
      remaining="$(( deadline - $(date +%s) ))"
      if [ "$remaining" -le 0 ]; then
        ROUTE_NOTE="dispatch budget expired before request"
        return 1
      fi
      # The intent must be durable before the request: a rerun reconciles from it.
      if ! rset ci.dispatch=requested ci.dispatch_at="$(now)" ci.dispatch_ref="$TARGET" ||
        [ "$(receipt_value ci.dispatch)" != requested ]; then
        ROUTE_NOTE="could not record the dispatch intent; nothing was dispatched"
        return 1
      fi
      remaining="$(( deadline - $(date +%s) ))"
      if [ "$remaining" -le 0 ]; then
        ROUTE_NOTE="dispatch intent recorded but budget expired; wait for reconciliation"
        return 1
      fi
      rc=0
      python3 "$ROOT/scripts/lib/run-bounded.py" "$remaining" 0.1 dispatch \
        "${OMS_GH_BIN:-gh}" workflow run test.yml --repo "$slug" --ref "$TARGET" >> "$LOG" 2>&1 || rc=$?
      if [ "$rc" -ne 0 ]; then
        rset ci.dispatch_rc="$rc"
        ROUTE_NOTE="dispatch result uncertain (exit $rc); wait for reconciliation"
        return 1
      fi
      rset ci.dispatch_rc=0
    fi
    [ "$(receipt_value ci.dispatch)" != requested ] || ROUTE_PENDING=1
    remaining="$(( deadline - $(date +%s) ))"
    if [ "$remaining" -le 0 ]; then
      if [ "$rid" = 0 ]; then ROUTE_NOTE="dispatched run has not appeared within --ci-wait"; else ROUTE_NOTE="dispatched run has not completed within --ci-wait"; fi
      return 0
    fi
    [ "$remaining" -le 30 ] || remaining=30
    sleep "$remaining"
    if [ "$(date +%s)" -ge "$deadline" ]; then
      if [ "$rid" = 0 ]; then ROUTE_NOTE="dispatched run has not appeared within --ci-wait"; else ROUTE_NOTE="dispatched run has not completed within --ci-wait"; fi
      return 0
    fi
  done
  rset ci.full_run_id="$rid" ci.full_conclusion="$concl"
  if [ "$concl" != success ]; then
    ROUTE_NOTE="dispatched run $rid is $concl; rerun it (gh run rerun $rid) and resume"
    return 1
  fi
  rc=0
  result="$(python3 "$ROOT/scripts/lib/ci-query.py" scope "$destination" "$rid" "$SHA" 30 2>> "$LOG")" || rc=$?
  result="$(oms_strip_cr "$result")"
  rset ci.full_scope_query_rc="$rc"
  if [ "$rc" -ne 0 ]; then
    rset ci.full_scope=unknown
    ROUTE_NOTE="dispatched run $rid has unreadable scope"
    return 1
  elif [ "$result" != full ]; then
    rset ci.full_scope="$result"
    ROUTE_NOTE="dispatched run $rid is not a full selection"
  else
    rset ci.full_scope=full ci.scope=full ci.scope_run_id="$rid" ci.dispatch_rc=""
    ROUTE_SCOPE=full
  fi
  return 0
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
  tree="$parent/wt-${parent##*.}"
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
  remote_retry() {
    local attempt=1
    while ! "$@"; do
      [ "$attempt" -lt 3 ] || return 1
      printf 'land: remote fetch failed (attempt %s/3); retrying\n' "$attempt" >&2
      if [ "$attempt" -eq 1 ]; then sleep 2; else sleep 5; fi
      attempt=$((attempt + 1))
    done
  }
  cd "$REPO"
  mkdir -p "$LAND_DIR"
  RECEIPT="$LAND_DIR/$STAMP.json" LOG="$LAND_DIR/$STAMP.log"
  SHA="$(git rev-parse HEAD)" SHORT="${SHA:0:7}"
  [ "$STAMP" = "$(request_stamp "$SHA")" ] || {
    note "error: HEAD or destination changed before job started"; return 1;
  }
  clean_tree || {
    note "error: tracked tree changed before job started"; return 1;
  }
  local resume=0 ci_only_resume=0 remote_sha failed_stages attempt marker
  if can_resume; then
    if can_handoff_only; then
      # The helper fetches the target ref and proves ancestry. Receipt-only
      # retries neither reinstall an old SHA nor rewrite imported bytes.
      after_land
      return $?
    fi
    remote_sha="$(remote_retry git ls-remote --exit-code "$(git remote get-url --push "$REMOTE")" "refs/heads/$TARGET")" || return 1
    remote_sha="${remote_sha%%[[:space:]]*}"
    if ! clean_tree; then
      note "error: cannot resume: tracked tree changed during remote-tip lookup"
      return 1
    fi
    if [ "$remote_sha" != "$SHA" ]; then
      if [ "$UPDATE" -eq 0 ] && can_resume_ci_only &&
          [ "$remote_sha" = "$(git rev-parse "$REMOTE/$TARGET" 2>/dev/null)" ] &&
          git merge-base --is-ancestor "$SHA" "$remote_sha"; then
        ci_only_resume=1
      else
        note "error: cannot resume: remote tip or tracked tree differs from verified commit"
        return 1
      fi
    fi
    if [ "$ci_only_resume" -eq 1 ] && [ "$CI_WAIT" -eq 0 ]; then
      note "error: cannot resume CI-only: --ci-wait 0 skips CI queries"
      return 1
    fi
    resume=1
  fi
  [ "$resume" -eq 1 ] || rset --reset
  # Net test and code lines per push, kept to judge test growth by evidence.
  [ "$ci_only_resume" -eq 1 ] || rset growth="$(oms_test_growth "$REMOTE/$TARGET" "$SHA")"
  rset pid="$$" state=running started_at="$(now)" sha="$SHA" gate.command="$GATE" \
    remote="$REMOTE" target="$TARGET" log="$LOG" request="$STAMP" resumed="$resume"
  attempt="$(($(grep -c '^land attempt: ' "$LOG" 2>/dev/null || true) + 1))"
  marker="land attempt: $attempt started_at=$(now) sha=$SHA"
  echo "$marker" >> "$LOG"
  # Detached startup is confirmed only after lock acquisition and the exact
  # SHA/tracked-tree admission checks above have written a running receipt.
  [ -z "$READY_FILE" ] || printf '0\n' > "$READY_FILE"
  local t0 rc=0 sample load_start load_end cpus load_note
  if [ "$resume" -eq 0 ]; then
    rset gate.rc=pending push.rc=pending ci.conclusion=skipped update.rc=skipped update.reason=not-reached
    if [ "$IGNORE_SIBLINGS" -eq 1 ]; then
      rset siblings.ignored=true
    else
      collect_live_siblings 1 || true
      [ -z "$SIBLING_LIVE" ] || rset siblings.live="$SIBLING_LIVE"
    fi
    t0="$(date +%s)"
    sample="$(host_load)"
    load_start="${sample%%:*}" cpus="${sample#*:}"
    [ -z "$cpus" ] || rset gate.cpus="$cpus"
    [ -z "$load_start" ] || rset gate.load_start="$load_start"
    run_gate || rc=$?
    sample="$(host_load)"
    load_end="${sample%%:*}"
    [ -z "$load_end" ] || rset gate.load_end="$load_end"
    rset gate.rc="$rc" gate.seconds="$(( $(date +%s) - t0 ))"
    if [ "$rc" -ne 0 ]; then
      # Name the failing stages and smoke tests: which tests ever catch
      # anything is the input for retiring the ones that never do, and the
      # ledger is queryable where scattered gate logs are not.
      failed_stages="$(awk -v marker="$marker" 'index($0, marker) { current=1; next } current { print }' "$LOG" |
        sed -n 's/^check: \([^ ]*\) FAILED.*/\1/p' | sort -u | tr '\n' ' ')"
      if awk -v marker="$marker" '
        index($0, marker) { current=1; next }
        current && /^check: stage deadline triggered$/ { found=1 }
        END { exit !found }
      ' "$LOG"; then
        failed_stages="${failed_stages}stage deadline "
      fi
      load_note=''
      if [ -n "$load_start" ] && [ -n "$load_end" ] && [ -n "$cpus" ]; then
        load_note=" (load $load_start->$load_end on $cpus CPUs)"
      fi
      "$ROOT/scripts/fail-ledger.sh" --repo "$REPO" record --kind verify --cmd "$GATE" \
        --exit "$rc" --summary "land: gate failed for $SHORT${failed_stages:+ in ${failed_stages% }}$load_note (see $LOG)" >/dev/null 2>&1 || true
      finish failed "gate exit $rc"; return 1
    fi
    # The gate runs outside the agent's shell, so the pass hook never clears its
    # earlier failure. Like that hook, resolve clears the command's fingerprint;
    # require the newest failed commit to be contained here, so a passing landing
    # of an unrelated line in a shared-ledger scratch worktree does not clear it.
    ledger_rc=0
    failed_sha="$("$ROOT/scripts/fail-ledger.sh" --repo "$REPO" check --cmd "$GATE" --ignore-state 2>&1 >/dev/null)" ||
      ledger_rc=$?
    if [ "$ledger_rc" -eq 3 ]; then
      failed_sha="$(printf '%s\n' "$failed_sha" | tr -d '\r' |
        sed -n 's/^fail-ledger: [0-9a-f]* already failed [0-9]*x (last exit [^)]*): land: gate failed for \([0-9a-f]\{7,40\}\).*$/\1/p' |
        head -n 1)"
      if [ -n "$failed_sha" ] && git merge-base --is-ancestor "$failed_sha" "$SHA" 2>/dev/null; then
        "$ROOT/scripts/fail-ledger.sh" --repo "$REPO" resolve --cmd "$GATE" \
          --how "land gate passed for $SHORT" >/dev/null 2>&1 || true
      fi
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
    remote_sha="$(remote_retry git ls-remote --exit-code "$(git remote get-url --push "$REMOTE")" "refs/heads/$TARGET")" || {
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
      record_attribution "$remote_sha"
    else
      # A reverify moved nothing: it proves the gate, never a landed range.
      rset push.rc=0 push.moved=0
    fi
  fi
  local conclusion=skipped run_id="" deadline remaining query_result
  if [ "$resume" -eq 1 ] && [ "$(receipt_value ci.conclusion)" = success ]; then
    conclusion=success
  elif [ "$ci_only_resume" -eq 0 ] && [ "$CI_WAIT" -gt 0 ] &&
      ! git -C "$REPO" ls-tree -r --name-only "$SHA" -- .github/workflows | grep -E '\.ya?ml$' > /dev/null; then
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
      if [ "$ci_scope" = full ]; then
        authority=ci.scope=full
      else
        # Default route to full proof: a dispatched full-selection run of this SHA.
        rc=0
        ci_full_route || rc=$?
        if [ "$rc" -ne 0 ]; then
          rset update.reason=ci-full-route-failed
          finish blocked "pushed $SHORT; ci success but affected-only; $ROUTE_NOTE; install needs full proof; resume with: $(land_retry_command)"
          return 1
        fi
        [ "$ROUTE_SCOPE" != full ] || authority=ci.scope=full
      fi
    fi
    if [ -z "$authority" ]; then
      rset update.reason=partial-gate
      local full_cmd="" resume_cmd local_gate="oms land --gate 'bash scripts/check.sh --parallel'"
      resume_cmd="$(land_retry_command)"
      if [ "$conclusion" = success ] && { [ "$ROUTE_PENDING" = 1 ] || [ "$(receipt_value ci.dispatch)" = requested ]; }; then
        finish blocked "pushed $SHORT; ci $conclusion; ${ROUTE_NOTE:-full run pending}; resume with: $resume_cmd; alternative: a full local gate, $local_gate"
        return 1
      elif [ "$conclusion" = success ]; then
        local destination_slug final_state=passed
        destination_slug="$(python3 "$ROOT/scripts/lib/ci-query.py" query-repo "$(git remote get-url --push "$REMOTE")")" || { finish blocked "invalid repository destination; retry to resume"; return 1; }
        printf -v full_cmd 'gh workflow run test.yml --repo %q --ref %q' "$destination_slug" "$TARGET"
        [ -z "$HANDOFF_TO" ] || final_state=blocked
        finish "$final_state" "pushed $SHORT; ci $conclusion but affected-only${ROUTE_NOTE:+ ($ROUTE_NOTE)}; install needs a full-selection CI run for this SHA: request it with: $full_cmd ; then resume with: $resume_cmd (or rerun with --ci-full to let land dispatch it once); alternative: a full local gate, $local_gate"
        [ "$final_state" = passed ] || return 1
      else
        finish passed "pushed $SHORT; ci $conclusion; install needs successful CI plus full proof: a full-selection CI run for this SHA, or a full local gate, $local_gate"
      fi
      return $?
    fi
    if [ "$conclusion" != success ]; then
      finish blocked "pushed $SHORT; install update requires successful CI (ci $conclusion)"
      return 1
    fi
    if [ "$resume" -eq 1 ] && [ "$(receipt_value update.rc)" = 0 ] &&
        [ "$(oms_strip_cr "$(receipt_value update.root)")" = "$install" ] &&
        [ "$(git -C "$install" rev-parse HEAD 2>/dev/null)" = "$SHA" ] &&
        installed_tree="$(git --no-optional-locks -C "$install" status --porcelain)" && [ -z "$installed_tree" ]; then
      finish passed "already pushed and installed $SHORT; ci $conclusion"
      return $?
    fi
    rc=0
    local deferred_at="" poll_left poll_step
    while :; do
      rc=0
      OMS_UPDATE_CALL_REPOS="$(printf '%s\n' "$REPO" "$HANDOFF_TO" "$install" "$(oms_state_root "$REPO")" "${SYNC_TO:-}")" \
        OH_MY_SETTING_UPDATE_EXPECTED_TARGET="$SHA" "$install/scripts/update.sh" >> "$LOG" 2>&1 || rc=$?
      [ "$rc" -eq 76 ] && [ "$WAIT" -eq 1 ] || break
      [ -n "$deferred_at" ] || deferred_at="$(date +%s)"
      poll_left=$(( SIBLING_WAIT - ($(date +%s) - deferred_at) ))
      [ "$poll_left" -gt 0 ] || break
      poll_step=15
      [ "$poll_left" -ge "$poll_step" ] || poll_step="$poll_left"
      printf 'install deferred (live workers); rechecking in %ss (%ss of --sibling-wait left)\n' "$poll_step" "$poll_left" >> "$LOG"
      sleep "$poll_step"
    done
    case "$rc" in
      0) update_reason=installed ;;
      76) update_reason=deferred-live-workers ;;
      *) update_reason="exit-$rc" ;;
    esac
    rset update.rc="$rc" update.reason="$update_reason" update.expected_sha="$SHA" update.root="$install" update.authority="$authority"
    if [ "$rc" -eq 76 ]; then
      local retry_command
      retry_command="$(land_retry_command)"
      finish failed "pushed; CI passed; installation deferred (live workers); retry: $retry_command"
      return 1
    fi
    if [ "$rc" -ne 0 ]; then
      finish failed "pushed $SHORT but install update exit $rc; ci $conclusion"
      return 1
    fi
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

# No cleanup trap: identity validation makes a crashed holder's record inert.
run_job_with_holder() {
  local rc=0
  oms_file_lock_set_current_identity || { run_job; return $?; }
  rm -f "$LAND_DIR/active-holder"
  python3 - "$LAND_DIR/active-holder" "$STAMP.json" "${STAMP%%-*}" \
    "$OMS_FILE_LOCK_CURRENT_PID" "$OMS_FILE_LOCK_CURRENT_START" <<'PYHOLDER' || { run_job; return $?; }
import json, os, sys, tempfile
path, receipt, sha, pid, start = sys.argv[1:]
fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), prefix=".holder.")
try:
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump(dict(receipt=receipt, sha=sha, pid=int(pid), start=start), f)
    os.replace(tmp, path)
finally:
    if os.path.exists(tmp):
        os.unlink(tmp)
PYHOLDER
  run_job || rc=$?
  rm -f "$LAND_DIR/active-holder"
  return "$rc"
}

show_status() {  # show_status [RECEIPT]; default: this HEAD's newest, else the newest
  local newest="${1:-}" active rc=0 head="" holder="" holder_pid="" holder_start="" current_start=""
  active="$(land_active)"
  if [ "$active" = true ]; then
    holder="$(cat "$LAND_DIR/active-holder" 2>/dev/null || true)"
    holder_pid="$(printf '%s' "$holder" | python3 -c 'import json,sys; r=json.load(sys.stdin); p=r["pid"]; assert type(p) is int and p > 0; print(p)' 2>/dev/null || true)"
    holder_start="$(printf '%s' "$holder" | python3 -c 'import json,sys; r=json.load(sys.stdin); assert isinstance(r["start"], str) and r["start"]; print(r["start"])' 2>/dev/null || true)"
    holder_pid="${holder_pid//$'\r'/}"
    holder_start="${holder_start//$'\r'/}"
    if [ -n "$holder_pid" ] && [ -n "$holder_start" ] && oms_file_lock_pid_alive "$holder_pid"; then
      current_start="$(oms_file_lock_process_start_token "$holder_pid" 2>/dev/null || true)"
      current_start="${current_start//$'\r'/}"
    fi
    [ -n "$current_start" ] && [ "$current_start" = "$holder_start" ] || holder=""
  fi
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
  python3 - "$newest" "$active" "$JSON" "${head:-}" "$holder" <<'PY' || return $?
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
try:
    holder = json.loads(sys.argv[5])
    if (not isinstance(holder, dict) or not isinstance(holder.get("receipt"), str)
            or not isinstance(holder.get("sha"), str)
            or len(holder["sha"]) not in (40, 64)
            or any(ch not in "0123456789abcdef" for ch in holder["sha"])):
        holder = {}
except (ValueError, TypeError):
    holder = {}
matches = bool(active is True and holder.get("receipt") == Path(sys.argv[1]).name
               and holder.get("sha") == r.get("sha"))
g, p, u, c = r.get("gate", {}), r.get("push", {}), r.get("update", {}), r.get("ci", {})
if sys.argv[3] == "1":
    r["active"] = active
    r.pop("step", None)
    r.pop("minutes", None)
    if active is True:
        r["active_matches_receipt"] = matches
        if holder.get("sha"):
            r["active_holder"] = holder["sha"]
    if matches:
        r["step"] = ("gate" if g.get("rc") == "pending" else "push" if p.get("rc") == "pending"
                     else "ci" if c.get("conclusion") == "pending"
                     or c.get("dispatch") == "requested" and not c.get("full_conclusion")
                     else "sync" if r.get("sync", {}).get("state") == "running" else "update")
        try:
            r["minutes"] = max(0, int((time.time() - calendar.timegm(time.strptime(
                r["started_at"], "%Y-%m-%dT%H:%M:%SZ"))) // 60))
        except (KeyError, ValueError, TypeError):
            pass
    print(json.dumps(r, ensure_ascii=False, sort_keys=True))
    sys.exit(0)
state = r.get("state")
receipt_state = state
if state == "running" and matches:
    state = "running (lock held)"
elif state == "running" and active is True:
    state = "receipt says running (lock held by another landing or unknown holder)"
elif state == "running" and active is False:
    state = "receipt says running, but no land holds the lock (stale receipt)"
elif state == "running" and active is None:
    state = "receipt says running, but lock activity is unknown"
print("land %s: %s" % (str(r.get("sha", ""))[:7], state))
if active is True and not matches:
    print("  lock is held by another landing or an unknown holder")
if sys.argv[4] and r.get("sha") != sys.argv[4]:
    print("  note: newest land in this repository, not this worktree's HEAD %s" % sys.argv[4][:7])
if receipt_state == "running" or active is True:
    print("  active: %s" % ("unknown" if active is None else str(active).lower()))
if u.get("rc") == "skipped" and state == "failed" and u.get("reason") == "not-reached":
    print("  install: not reached (land failed before install)")
elif u.get("rc") == "skipped":
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
      trap land_ready_exit EXIT
    fi
    rc=0
    oms_with_file_lock "$LAND_DIR/active" run_job_with_holder || rc=$?
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
# FETCH_HEAD lives in .git/worktrees/<id> for linked callers; only the target
# remote-tracking ref is needed here, so leave that protected registry untouched.
attempt=1
while ! git -C "$REPO" -c maintenance.auto=false -c gc.auto=0 -c fetch.writeCommitGraph=false fetch -q --no-write-fetch-head --no-tags --no-recurse-submodules -- "$REMOTE" "+refs/heads/$TARGET:refs/remotes/$REMOTE/$TARGET"; do
  [ "$attempt" -lt 3 ] || { echo "error: fetch from $REMOTE failed" >&2; exit 2; }
  printf 'land: remote fetch failed (attempt %s/3); retrying\n' "$attempt" >&2
  if [ "$attempt" -eq 1 ]; then sleep 2; else sleep 5; fi
  attempt=$((attempt + 1))
done
SHA="$(git -C "$REPO" rev-parse HEAD)"
# Retries/worktrees share a receipt; different destinations/gates never share proof.
STAMP="$(request_stamp "$SHA")"
RECEIPT="$LAND_DIR/$STAMP.json"
# Only receipt handoff and CI-only resume may use an ancestor of the
# destination tip; both are bound to the freshly fetched target ref.
if ! can_handoff_only && ! can_resume_ci_only &&
    ! git -C "$REPO" merge-base --is-ancestor "$REMOTE/$TARGET" "$SHA"; then
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
[ "$WAIT" -eq 0 ] || job+=(--wait)
[ "$IGNORE_SIBLINGS" -eq 0 ] || job+=(--ignore-siblings)
[ "$CI_FULL" -eq 0 ] || job+=(--ci-full)
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
