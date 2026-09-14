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

usage() {
  cat <<'EOF'
Usage: land.sh [--repo PATH] [--remote NAME] [--target BRANCH] [--gate CMD]
               [--wait] [--no-update] [--ci-wait SECONDS]
               [--sibling-wait SECONDS] [--ignore-siblings]
       land.sh status [--repo PATH] [--json]

Preconditions: a clean tracked tree, HEAD ahead of REMOTE/TARGET with the
remote tip as an ancestor (rebase first otherwise), and a gate command
(default: bash scripts/check.sh when the repo has one).
Stages, each recorded beside its gate log under
$XDG_STATE_HOME/oh-my-setting/land/<repo-slug>/<sha>-<request-digest>.json,
outside the repo:
  gate    the gate command; a failure is recorded in the fail ledger
  push    git push --no-verify REMOTE VERIFIED_SHA:TARGET, only if HEAD and the tree
          are unchanged since the gate started
  ci      the GitHub run for the pushed commit, polled up to --ci-wait
          seconds (default 1500; 0 skips; needs gh)
  update  only after CI success, the matching install checkout's update.sh;
          its target must still be the verified SHA. Skipped CI never updates.
  sibling live autopilot runs (proposing, proposal-review, driving) in sibling
          worktrees are reported at intake and waited for before push, up to
          --sibling-wait seconds (default 1800; 0 checks once and blocks at
          once, it does not skip: --ignore-siblings skips)
Without --wait the job detaches (setsid) and this prints the receipt path.
Retrying the same commit/destination/gate reuses successful push evidence and
resumes CI/install only. Concurrent jobs for this repository exit 75.
EOF
}

REPO="$PWD" REMOTE=origin TARGET=main GATE="" WAIT=0 UPDATE=1 CI_WAIT=1500
SIBLING_WAIT=1800 IGNORE_SIBLINGS=0
MODE=start JSON=0 STAMP=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    status) MODE=status; shift ;;
    --run-job) MODE=job; STAMP="$2"; shift 2 ;;
    --repo) REPO="$2"; shift 2 ;;
    --remote) REMOTE="$2"; shift 2 ;;
    --target) TARGET="$2"; shift 2 ;;
    --gate) GATE="$2"; shift 2 ;;
    --wait) WAIT=1; shift ;;
    --no-update) UPDATE=0; shift ;;
    --ci-wait) CI_WAIT="$2"; shift 2 ;;
    --sibling-wait) SIBLING_WAIT="$2"; shift 2 ;;
    --ignore-siblings) IGNORE_SIBLINGS=1; shift ;;
    --json) JSON=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done
case "$CI_WAIT" in ''|*[!0-9]*) echo "error: --ci-wait must be seconds" >&2; exit 2 ;; esac
case "$SIBLING_WAIT" in ''|*[!0-9]*) echo "error: --sibling-wait must be seconds" >&2; exit 2 ;; esac
REPO="$(git -C "$REPO" rev-parse --show-toplevel 2>/dev/null)" ||
  { echo "error: --repo is not a git checkout: $REPO" >&2; exit 2; }
REPO="$(oms_strip_cr "$REPO")"
REPO="$(cd "$REPO" 2>/dev/null && pwd -P)" ||
  { echo "error: cannot resolve --repo path" >&2; exit 2; }

land_state_dir() {  # one durable, safe state directory for all linked worktrees
  local common_dir common_root name digest

  common_dir="$(git -C "$REPO" rev-parse --git-common-dir 2>/dev/null)" || return 1
  common_dir="$(oms_strip_cr "$common_dir")"
  case "$common_dir" in
    /*|[A-Za-z]:/*) ;;
    *) common_dir="$REPO/$common_dir" ;;
  esac
  common_dir="$(cd "$common_dir" 2>/dev/null && pwd -P)" || return 1
  common_root="$(cd "$common_dir/.." 2>/dev/null && pwd -P)" || return 1
  name="$(basename "$common_root")"
  name="$(printf '%s' "$name" | tr -c 'A-Za-z0-9._-' '_')"
  name="${name//../_}"
  case "$name" in ''|.|..) name=repo ;; esac
  digest="$(printf '%s' "$common_root" | cksum | awk '{print $1 "-" $2}')"
  case "$digest" in ''|*[!0-9-]*) return 1 ;; esac
  printf '%s/%s-%s\n' "${XDG_STATE_HOME:-$HOME/.local/state}/oh-my-setting/land" "$name" "$digest"
}

LAND_DIR="$(land_state_dir)" ||
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

bounded_ci_query() {  # seconds, gh arguments
  local seconds="$1"; shift
  if command -v timeout >/dev/null 2>&1; then
    timeout --kill-after=1 "$seconds" gh "$@"
  elif command -v gtimeout >/dev/null 2>&1; then
    gtimeout --kill-after=1 "$seconds" gh "$@"
  else
    python3 "$ROOT/scripts/lib/run-bounded.py" "$seconds" 1 land-ci gh "$@"
  fi
}

run_job() {
  cd "$REPO"
  mkdir -p "$LAND_DIR"
  RECEIPT="$LAND_DIR/$STAMP.json" LOG="$LOG_DIR/$STAMP.log"
  SHA="$(git rev-parse HEAD)" SHORT="${SHA:0:7}"
  [ "$STAMP" = "$(request_stamp "$SHA")" ] || {
    note "error: HEAD or destination changed before job started"; return 1;
  }
  local resume=0 remote_sha
  if can_resume; then
    remote_sha="$(git ls-remote --exit-code "$(git remote get-url --push "$REMOTE")" "refs/heads/$TARGET")" || return 1
    remote_sha="${remote_sha%%[[:space:]]*}"
    if [ "$remote_sha" != "$SHA" ] || ! clean_tree; then
      note "error: cannot resume: remote tip or tracked tree differs from verified commit"
      return 1
    fi
    resume=1
  fi
  [ "$resume" -eq 1 ] || rset --reset
  rset pid="$$" state=running started_at="$(now)" sha="$SHA" gate.command="$GATE" \
    remote="$REMOTE" target="$TARGET" log="$LOG" request="$STAMP" resumed="$resume"
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
    bash -c "$GATE" >> "$LOG" 2>&1 || rc=$?
    rset gate.rc="$rc" gate.seconds="$(( $(date +%s) - t0 ))"
    if [ "$rc" -ne 0 ]; then
      "$ROOT/scripts/fail-ledger.sh" --repo "$REPO" record --kind verify --cmd "$GATE" \
        --exit "$rc" --summary "land: gate failed for $SHORT (see $LOG)" >/dev/null 2>&1 || true
      finish failed "gate exit $rc"; return 1
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
    rc=0
    git push --no-verify "$REMOTE" "$SHA:refs/heads/$TARGET" >> "$LOG" 2>&1 || rc=$?
    rset push.rc="$rc"
    [ "$rc" -eq 0 ] || { finish failed "push exit $rc"; return 1; }
  fi
  local conclusion=skipped run_id="" deadline remaining query_result
  if [ "$resume" -eq 1 ] && [ "$(receipt_value ci.conclusion)" = success ]; then
    conclusion=success
  elif [ "$CI_WAIT" -gt 0 ] && command -v gh >/dev/null 2>&1; then
    conclusion=timeout
    deadline="$(( $(date +%s) + CI_WAIT ))"
    while [ "$(date +%s)" -lt "$deadline" ]; do
      local ci_args=(--repo "$(repo_slug "$(git remote get-url --push "$REMOTE")")"
        --commit "$SHA" --branch "$TARGET" --event push --limit 1)
      [ ! -f "$REPO/.github/workflows/test.yml" ] || ci_args+=(--workflow test.yml)
      remaining="$(( deadline - $(date +%s) ))"
      [ "$remaining" -gt 0 ] || break
      [ "$remaining" -le 30 ] || remaining=30
      rc=0
      query_result="$(bounded_ci_query "$remaining" run list "${ci_args[@]}" \
        --json databaseId,status,conclusion \
        --jq '.[] | "\(.databaseId) \(if .status == "completed" then .conclusion else "pending" end)"' \
        2>> "$LOG")" || rc=$?
      rset ci.query_rc="$rc"
      if [ "$rc" -ne 0 ]; then
        rset ci.conclusion=query-error
        finish failed "pushed $SHORT but CI query exit $rc (see $LOG); retry to resume"
        return 1
      fi
      query_result="$(oms_strip_cr "$query_result")"
      read -r run_id conclusion <<< "$query_result"
      [ -n "$run_id" ] && [ "$conclusion" != pending ] && break
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
  local install
  if [ "$UPDATE" -eq 1 ] && install="$(install_root)"; then
    if [ "$conclusion" != success ]; then
      finish blocked "pushed $SHORT; install update requires successful CI (ci $conclusion)"
      return 1
    fi
    if [ "$resume" -eq 1 ] && [ "$(receipt_value update.rc)" = 0 ] &&
        [ "$(oms_strip_cr "$(receipt_value update.root)")" = "$install" ]; then
      finish passed "already pushed and installed $SHORT; ci $conclusion"
      return 0
    fi
    rc=0
    OH_MY_SETTING_UPDATE_EXPECTED_TARGET="$SHA" "$install/scripts/update.sh" >> "$LOG" 2>&1 || rc=$?
    rset update.rc="$rc" update.expected_sha="$SHA" update.root="$install"
    if [ "$rc" -ne 0 ]; then
      finish failed "pushed $SHORT but install update exit $rc; ci $conclusion"
      return 1
    fi
  fi
  finish passed "pushed $SHORT to $REMOTE/$TARGET; ci $conclusion"
}

show_status() {  # show_status [RECEIPT]; default: the most recently written one
  local newest="${1:-}"
  [ -n "$newest" ] || newest="$(ls -t "$LAND_DIR"/*.json 2>/dev/null | head -n 1)" || true
  [ -n "$newest" ] || { echo "no landing recorded under $LAND_DIR"; return 1; }
  if [ "$JSON" -eq 1 ]; then cat "$newest"; return 0; fi
  python3 - "$newest" <<'PY'
import json, sys
r = json.load(open(sys.argv[1], encoding="utf-8"))
g, p, u, c = r.get("gate", {}), r.get("push", {}), r.get("update", {}), r.get("ci", {})
print("land %s: %s" % (str(r.get("sha", ""))[:7], r.get("state")))
print("  gate: %s (%ss)  push: %s  update: %s  ci: %s%s" % (
    "ok" if g.get("rc") == 0 else g.get("rc", "-"), g.get("seconds", "-"),
    "ok" if p.get("rc") == 0 else p.get("rc", "-"), "ok" if u.get("rc") == 0 else u.get("rc", "-"),
    c.get("conclusion", "-"), " (run %s)" % c["run_id"] if c.get("run_id") else ""))
if r.get("summary"):
    print("  %s" % r["summary"])
s = r.get("siblings", {})
if s.get("ignored"):
    print("  siblings: ignored")
elif s.get("live"):
    print("  siblings: %s (waited %ss)" % (s["live"], p.get("sibling_wait_seconds", "-")))
print("  log: %s" % r.get("log", ""))
PY
}

case "$MODE" in
  status) show_status; exit ;;
  job)
    rc=0
    oms_try_file_lock "$LAND_DIR/active" run_job || rc=$?
    [ "$rc" -ne 75 ] || echo "land already active for this repository; use land status" >&2
    exit "$rc"
    ;;
esac

clean_tree || { echo "error: commit or stash tracked changes first" >&2; exit 2; }
if [ -z "$GATE" ]; then
  [ -f "$REPO/scripts/check.sh" ] || { echo "error: no scripts/check.sh here; pass --gate CMD" >&2; exit 2; }
  GATE="bash scripts/check.sh"
fi
git -C "$REPO" fetch -q "$REMOTE" || { echo "error: fetch from $REMOTE failed" >&2; exit 2; }
if ! git -C "$REPO" merge-base --is-ancestor "$REMOTE/$TARGET" HEAD; then
  echo "error: $REMOTE/$TARGET is not an ancestor of HEAD; rebase first" >&2; exit 2
fi
SHA="$(git -C "$REPO" rev-parse HEAD)"
# Retries/worktrees share a receipt; different destinations/gates never share proof.
STAMP="$(request_stamp "$SHA")"
RECEIPT="$LAND_DIR/$STAMP.json"
if [ "$(git -C "$REPO" rev-parse HEAD)" = "$(git -C "$REPO" rev-parse "$REMOTE/$TARGET")" ]; then
  if ! can_resume; then
    echo "nothing to land: HEAD is already $REMOTE/$TARGET; no matching push receipt to resume"; exit 0
  fi
fi
mkdir -p "$LAND_DIR"
job=("$ROOT/scripts/land.sh" --run-job "$STAMP" --repo "$REPO" --remote "$REMOTE" \
  --target "$TARGET" --gate "$GATE" --ci-wait "$CI_WAIT" --sibling-wait "$SIBLING_WAIT")
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
set -m
setsid bash "${job[@]}" < /dev/null > /dev/null 2>&1 &
set +m
echo "landing $(git -C "$REPO" rev-parse --short HEAD) in the background"
echo "receipt: $LAND_DIR/$STAMP.json"
echo "status: oms land status --repo $REPO"
