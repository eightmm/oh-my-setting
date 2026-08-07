#!/usr/bin/env bash
set -euo pipefail

# Print the CI conclusion for a branch's HEAD (default: current branch) so a
# red run can't go unnoticed after a push — the "nobody looked" failure mode.
# The result is keyed by (branch, HEAD sha): a run for an earlier commit is
# history, an unpushed HEAD is reported as unpushed rather than dressed up in
# an older commit's verdict, and a gh that cannot answer says so instead of
# reading as "no CI here". Exits nonzero when HEAD's completed run failed, and
# 2 when gh is missing or unusable. gh is overridable
# (OMS_GH_BIN) for testing. `record` also appends the conclusion to
# .oms/ci.jsonl so a later session / oms state sees "CI failed on <sha>"
# instead of the result vanishing after the print. `tick` is the bounded,
# fail-open session-boundary observer: it skips a completed current SHA and
# throttles pending lookups instead of polling in the foreground.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"
# shellcheck source=scripts/lib/file-lock.sh
. "$ROOT/scripts/lib/file-lock.sh"
# shellcheck source=scripts/lib/work-journal.sh
. "$ROOT/scripts/lib/work-journal.sh"

GH="${OMS_GH_BIN:-gh}"
ACTION="status"
branch=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    status) ACTION="status"; shift ;;
    record|--record) ACTION="record"; shift ;;
    tick) ACTION="tick"; shift ;;
    -h|--help)
      cat <<'EOF'
Usage: ci-status.sh [status|record|tick] [branch]

status  Print the latest CI run and fail on a completed red result (default).
record  Also append the result to .oms/ci.jsonl and Work Journal.
tick    Fail-open bounded refresh for prompt/session boundaries. A completed
        current SHA and a recent pending lookup are local no-ops.

Environment:
  OMS_CI_TICK_INTERVAL  Minimum seconds between unresolved lookups (default 120).
  OMS_CI_TICK_TIMEOUT   Network subprocess budget in seconds (default 2).
EOF
      exit 0
      ;;
    *)
      [ -z "$branch" ] || { echo "error: unexpected argument: $1" >&2; exit 2; }
      branch="$1"
      shift
      ;;
  esac
done
if [ -z "$branch" ]; then
  branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo main)"
fi

if [ "$ACTION" = tick ]; then
  # A hook must not adopt a plain repository or fail an agent turn because gh
  # is unavailable. The explicit status/record surfaces retain their errors.
  state_root="$(oms_repo_root "$PWD")"
  [ -d "$state_root/.oms" ] || exit 0
  command -v "$GH" >/dev/null 2>&1 || exit 0
  LEDGER="${OMS_CI_LEDGER:-$state_root/.oms/ci.jsonl}"
  marker="$state_root/.oms/hooks/ci-tick"
  interval="${OMS_CI_TICK_INTERVAL:-120}"
  timeout_seconds="${OMS_CI_TICK_TIMEOUT:-2}"
  case "$interval" in *[!0-9]*|"") interval=120 ;; esac
  case "$timeout_seconds" in *[!0-9]*|"") timeout_seconds=2 ;; esac
  [ "$timeout_seconds" -gt 0 ] || timeout_seconds=2
  mkdir -p "$(dirname "$marker")"

  ci_tick_locked() {
    local head
    head="$(git -C "$state_root" rev-parse HEAD 2>/dev/null || true)"
    [ -n "$head" ] || return 0
    if ! python3 - "$LEDGER" "$marker" "$head" "$interval" <<'PY'
import json, os, sys, time
ledger, marker, head, interval = sys.argv[1:]
latest = None
try:
    with open(ledger, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if isinstance(row, dict):
                latest = row
except OSError:
    pass
if latest and latest.get("sha") == head and latest.get("status") == "completed":
    raise SystemExit(1)
try:
    if time.time() - os.path.getmtime(marker) < int(interval):
        raise SystemExit(1)
except OSError:
    pass
with open(marker, "w", encoding="utf-8") as handle:
    handle.write(str(int(time.time())) + "\n")
PY
    then
      return 0
    fi

    # Python supplies the portable timeout missing from stock macOS userland.
    # A timeout, red run, or transient network error is retried at a later
    # boundary and never blocks the prompt/Stop hook that called tick.
    python3 - "$timeout_seconds" "$ROOT/scripts/ci-status.sh" "$branch" <<'PY' || true
import os, subprocess, sys
timeout, script, branch = int(sys.argv[1]), sys.argv[2], sys.argv[3]
stdout = subprocess.DEVNULL if os.environ.get("OMS_CI_TICK_QUIET", "1") == "1" else None
stderr = subprocess.DEVNULL if os.environ.get("OMS_CI_TICK_QUIET", "1") == "1" else None
try:
    subprocess.run(
        ["bash", script, "record", branch],
        stdin=subprocess.DEVNULL,
        stdout=stdout,
        stderr=stderr,
        timeout=timeout,
        check=False,
        env=os.environ.copy(),
    )
except (OSError, subprocess.TimeoutExpired):
    pass
PY
  }
  # Prompt and Stop hooks must never queue behind one another. The first tick
  # owns the short network budget; concurrent boundaries skip immediately and
  # let the next natural boundary retry.
  oms_try_file_lock "$marker" ci_tick_locked || true
  exit 0
fi

command -v "$GH" >/dev/null 2>&1 || { echo "ci-status: gh CLI not installed" >&2; exit 2; }

# Whether HEAD is on the upstream decides which question CI can even answer.
# It is only knowable for the checked-out branch with a configured upstream;
# for any other branch the recorded-vs-HEAD comparison stays the whole story.
HEAD_SHA="$(git rev-parse HEAD 2>/dev/null || true)"
CURRENT_BRANCH="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || true)"
AHEAD=""
if [ -n "$HEAD_SHA" ] && [ "$branch" = "$CURRENT_BRANCH" ]; then
  AHEAD="$(git rev-list --count '@{u}..HEAD' 2>/dev/null || true)"
fi
case "$AHEAD" in *[!0-9]*) AHEAD="" ;; esac

# An unauthenticated gh prints nothing and exits nonzero. Swallowing that made
# the query indistinguishable from "this branch has no CI" — an all-clear this
# tool exists to prevent. `tick` already returned above: a hook stays silent.
# One commit routinely has several runs, so ask for enough rows to recognise
# HEAD's own run in the same single call rather than only the newest one.
gh_rc=0
json="$("$GH" run list --branch "$branch" --limit 10 \
  --json status,conclusion,workflowName,headSha,url 2>/dev/null)" || gh_rc=$?
if [ "$gh_rc" -ne 0 ]; then
  echo "ci-status: gh not authenticated or unreachable (try: gh auth login)" >&2
  exit 2
fi

LEDGER=""
PR_JSON=""
PR_ERR=""
JOURNAL_ROW=""
GH_DEGRADED=0
cleanup_ci_status() {
  [ -z "${JOURNAL_ROW:-}" ] || rm -f "$JOURNAL_ROW"
  [ -z "${PR_ERR:-}" ] || rm -f "$PR_ERR"
}
trap cleanup_ci_status EXIT
if [ "$ACTION" = record ]; then
  state_root="$(oms_repo_root "$PWD")"
  LEDGER="${OMS_CI_LEDGER:-$state_root/.oms/ci.jsonl}"
  mkdir -p "$(dirname "$LEDGER")"
  agent_memory_ensure_oms_ignore_for_path "$LEDGER" 2>/dev/null || true
  PR_ERR="$(mktemp)" || exit 1
  pr_rc=0
  PR_JSON="$("$GH" pr view "$branch" --json number,state,url,headRefOid 2>"$PR_ERR")" || pr_rc=$?
  if [ "$pr_rc" -ne 0 ]; then
    PR_JSON=""
    # "no pull requests found for branch" is the ordinary answer for a branch
    # without a PR. Any other failure is the same gh outage as above, reported
    # rather than silently dropped — but only after the CI row is written.
    if ! grep -qi 'pull request' "$PR_ERR"; then
      echo "ci-status: gh not authenticated or unreachable (try: gh auth login)" >&2
      GH_DEGRADED=1
    fi
  fi
  JOURNAL_ROW="$(mktemp)" || exit 1
fi

ci_rc=0
OMS_BRANCH="$branch" OMS_JSON="$json" OMS_CI_LEDGER_OUT="$LEDGER" \
  OMS_CI_PR_JSON="$PR_JSON" OMS_CI_JOURNAL_ROW="$JOURNAL_ROW" \
  OMS_CI_HEAD="$HEAD_SHA" OMS_CI_AHEAD="$AHEAD" \
  OMS_CI_TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)" python3 - <<'PY' || ci_rc=$?
import json, os, sys
branch = os.environ["OMS_BRANCH"]
raw = os.environ.get("OMS_JSON", "").strip()
head = os.environ.get("OMS_CI_HEAD", "")
ahead_raw = os.environ.get("OMS_CI_AHEAD", "")
ahead = int(ahead_raw) if ahead_raw.isdigit() else None
try:
    runs = json.loads(raw) if raw else []
except Exception:
    runs = []
if not isinstance(runs, list):
    runs = []
runs = [r for r in runs if isinstance(r, dict)]
latest = runs[0] if runs else None
match = None
for r in runs:
    if head and (r.get("headSha") or "") == head:
        match = r
        break


def describe(r):
    status = r.get("status") or "?"
    return "%s %s" % (status, r.get("conclusion") or status)


def history(r):
    return "  history: %s on %s  %s" % (
        describe(r), (r.get("headSha") or "?")[:12], r.get("url", ""))


# `current` is the run that actually describes HEAD; `chosen` is the newest
# run worth recording as history. Printing a prior commit's green run as the
# current result is exactly the reassurance this tool must never manufacture.
if ahead is not None and ahead > 0:
    print("ci-status [%s]: unpushed (%d commit%s ahead of the upstream — push to get CI)" % (
        branch, ahead, "" if ahead == 1 else "s"))
    if latest:
        print(history(latest))
    chosen, current = latest, None
elif match:
    print("ci-status [%s]: %s  %s" % (branch, describe(match), match.get("url", "")))
    chosen, current = match, match
elif ahead == 0 and latest:
    print("ci-status [%s]: no run yet for HEAD %s (pending or not started)" % (
        branch, head[:12]))
    print(history(latest))
    chosen, current = latest, None
elif latest:
    # Push state unknown (no upstream, or a branch other than the checkout):
    # the newest run remains the only answer available, as before.
    print("ci-status [%s]: %s  %s" % (branch, describe(latest), latest.get("url", "")))
    chosen, current = latest, latest
else:
    print("ci-status: no runs for %s" % branch)
    chosen, current = None, None

ledger = os.environ.get("OMS_CI_LEDGER_OUT", "")
if ledger and chosen:
    r = chosen
    status = r.get("status")
    concl = r.get("conclusion") or status
    sha = r.get("headSha", "")
    row = {"schema": 1, "ts": os.environ["OMS_CI_TS"], "branch": branch,
           "sha": sha, "status": status, "conclusion": concl, "url": r.get("url", "")}
    try:
        pr = json.loads(os.environ.get("OMS_CI_PR_JSON", "") or "{}")
    except Exception:
        pr = {}
    if isinstance(pr, dict) and pr.get("number"):
        if not pr.get("headRefOid") or not sha or pr.get("headRefOid") == sha:
            row["pr_number"] = pr.get("number")
            row["pr_state"] = pr.get("state")
            row["pr_url"] = pr.get("url")
    # Dedupe: skip if the last row is the same sha with the same conclusion.
    prev = None
    if os.path.isfile(ledger):
        for line in open(ledger, encoding="utf-8", errors="replace"):
            line = line.strip()
            if line:
                try:
                    prev = json.loads(line)
                except Exception:
                    pass
    if not (prev and prev.get("sha") == sha and prev.get("conclusion") == concl):
        with open(ledger, "a", encoding="utf-8") as f:
            f.write(json.dumps(row, ensure_ascii=False) + "\n")
    journal_row = os.environ.get("OMS_CI_JOURNAL_ROW", "")
    if journal_row and status == "completed":
        with open(journal_row, "w", encoding="utf-8", newline="\n") as f:
            f.write(json.dumps(row, ensure_ascii=False) + "\n")

# Nonzero only on a completed failure for the commit in hand; in-progress and
# queued are not failures, and neither is a red run on a superseded commit.
verdict = (current or {}).get("conclusion") or (current or {}).get("status")
sys.exit(1 if verdict in ("failure", "timed_out", "cancelled", "startup_failure") else 0)
PY

if [ -n "$JOURNAL_ROW" ] && [ -s "$JOURNAL_ROW" ]; then
  work_journal_observe "$state_root" ci-status "$JOURNAL_ROW" \
    --record-path "$LEDGER"
fi
# A gh failure must not read as a clean run even when the CI row itself landed.
if [ "$ci_rc" -eq 0 ] && [ "$GH_DEGRADED" -eq 1 ]; then
  ci_rc=2
fi
exit "$ci_rc"
