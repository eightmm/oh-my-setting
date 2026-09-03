#!/usr/bin/env bash
set -euo pipefail

# Land a committed worktree on the shared branch as one detached job. It runs
# the local gate, pushes without the pre-push hook, refreshes the install when
# the repo is the harness checkout itself, waits for CI, and writes a receipt;
# an agent starts it and reads `oms land status` instead of babysitting the gate.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/install-contract.sh
. "$ROOT/scripts/lib/install-contract.sh"
# shellcheck source=scripts/lib/work-journal.sh
. "$ROOT/scripts/lib/work-journal.sh"

usage() {
  cat <<'EOF'
Usage: land.sh [--repo PATH] [--remote NAME] [--target BRANCH] [--gate CMD]
               [--wait] [--no-update] [--ci-wait SECONDS]
       land.sh status [--repo PATH] [--json]

Preconditions: a clean tracked tree, HEAD ahead of REMOTE/TARGET with the
remote tip as an ancestor (rebase first otherwise), and a gate command
(default: bash scripts/check.sh when the repo has one).
Stages, each recorded beside its gate log under
$XDG_STATE_HOME/oh-my-setting/land/<repo-slug>/<stamp>-<sha>-<pid>.json,
outside the repo:
  gate    the gate command; a failure is recorded in the fail ledger
  push    git push --no-verify REMOTE HEAD:TARGET, only if HEAD and the tree
          are unchanged since the gate started
  update  the install checkout's update.sh, only when REMOTE is that
          checkout's origin (so a push there is what the install pulls)
  ci      the GitHub run for the pushed commit, polled up to --ci-wait
          seconds (default 1500; 0 skips; needs gh)
Without --wait the job detaches (setsid) and this prints the receipt path.
EOF
}

REPO="$PWD" REMOTE=origin TARGET=main GATE="" WAIT=0 UPDATE=1 CI_WAIT=1500
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
    --json) JSON=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done
case "$CI_WAIT" in ''|*[!0-9]*) echo "error: --ci-wait must be seconds" >&2; exit 2 ;; esac
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

run_job() {
  cd "$REPO"
  mkdir -p "$LAND_DIR"
  RECEIPT="$LAND_DIR/$STAMP.json" LOG="$LOG_DIR/$STAMP.log"
  SHA="$(git rev-parse HEAD)" SHORT="${SHA:0:7}"
  rset pid="$$" state=running started_at="$(now)" sha="$SHA" gate.command="$GATE" \
    remote="$REMOTE" target="$TARGET" log="$LOG"
  local t0 rc=0
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
  rc=0
  git push --no-verify "$REMOTE" "HEAD:refs/heads/$TARGET" >> "$LOG" 2>&1 || rc=$?
  rset push.rc="$rc"
  [ "$rc" -eq 0 ] || { finish failed "push exit $rc"; return 1; }
  local install
  if [ "$UPDATE" -eq 1 ] && install="$(install_root)"; then
    rc=0
    "$install/scripts/update.sh" >> "$LOG" 2>&1 || rc=$?
    rset update.rc="$rc"
  else
    rset update.rc=skipped
  fi
  local conclusion=skipped run_id="" deadline
  if [ "$CI_WAIT" -gt 0 ] && command -v gh >/dev/null 2>&1; then
    conclusion=timeout
    deadline="$(( $(date +%s) + CI_WAIT ))"
    while [ "$(date +%s)" -lt "$deadline" ]; do
      read -r run_id conclusion < <(gh run list --commit "$SHA" --limit 1 \
        --json databaseId,status,conclusion \
        --jq '.[] | "\(.databaseId) \(if .status == "completed" then .conclusion else "pending" end)"' \
        2>/dev/null || echo "")
      [ -n "$run_id" ] && [ "$conclusion" != pending ] && break
      conclusion=timeout
      sleep 30
    done
    rset ci.run_id="${run_id:-}" ci.conclusion="$conclusion"
  else
    rset ci.conclusion=skipped
  fi
  case "$conclusion" in
    success|skipped) finish passed "pushed $SHORT to $REMOTE/$TARGET; ci $conclusion" ;;
    *) finish failed "pushed $SHORT but ci $conclusion"; return 1 ;;
  esac
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
print("  log: %s" % r.get("log", ""))
PY
}

case "$MODE" in
  status) show_status; exit ;;
  job) run_job || exit 1; exit 0 ;;
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
if [ "$(git -C "$REPO" rev-parse HEAD)" = "$(git -C "$REPO" rev-parse "$REMOTE/$TARGET")" ]; then
  echo "nothing to land: HEAD is already $REMOTE/$TARGET"; exit 0
fi
mkdir -p "$LAND_DIR"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)-$(git -C "$REPO" rev-parse --short HEAD)-$$"
job=("$ROOT/scripts/land.sh" --run-job "$STAMP" --repo "$REPO" --remote "$REMOTE" \
  --target "$TARGET" --gate "$GATE" --ci-wait "$CI_WAIT")
[ "$UPDATE" -eq 1 ] || job+=(--no-update)
if [ "$WAIT" -eq 1 ]; then
  rc=0
  bash "${job[@]}" || rc=$?
  JSON=0 show_status "$LAND_DIR/$STAMP.json"
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
