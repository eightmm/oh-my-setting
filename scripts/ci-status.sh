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
# throttles pending lookups instead of polling in the foreground. `protection`
# answers the question a quick push hook rests on: is CI's `gate` job a required
# check on the destination branch? It only reads GitHub, runs only when asked,
# and has three answers — a lookup that could not read a source is unknown,
# never absent and never a pass.

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
limit=20
while [ "$#" -gt 0 ]; do
  case "$1" in
    status) ACTION="status"; shift ;;
    record|--record) ACTION="record"; shift ;;
    tick) ACTION="tick"; shift ;;
    history) ACTION="history"; shift ;;
    protection) ACTION="protection"; shift ;;
    --limit)
      [ "$#" -ge 2 ] || { echo "error: --limit needs a positive integer" >&2; exit 2; }
      limit="$2"
      case "$limit" in *[!0-9]*|"") echo "error: --limit must be a positive integer" >&2; exit 2 ;; esac
      [ "$limit" -gt 0 ] || { echo "error: --limit must be a positive integer" >&2; exit 2; }
      shift 2
      ;;
    -h|--help)
      cat <<'EOF'
Usage: ci-status.sh [status|record|tick] [branch]
       ci-status.sh history [--limit N] [branch]
       ci-status.sh protection [branch]

status  Print the latest CI run and fail on a completed red result (default).
record  Also append the result to .oms/ci.jsonl and Work Journal.
tick    Fail-open bounded refresh for prompt/session boundaries. A completed
        current SHA and a recent pending lookup are local no-ops.
history Show recent completed runs and ranked failing jobs/tests (read-only).
protection
        Report whether CI job 'gate' is a required check on the branch
        (default: the repository's default branch), from branch protection
        and active rulesets (read-only GET requests; nothing is changed).
        Exit 0 proven, 1 absent, 2 unknown: a source that could not be read
        (no gh, no permission, timeout) is unknown, never absent or a pass.

Environment:
  OMS_CI_TICK_INTERVAL  Minimum seconds between unresolved lookups (default 120).
  OMS_CI_TICK_TIMEOUT   Network subprocess budget in seconds (default 2).
  OMS_CI_PROTECTION_TIMEOUT
                        Seconds each protection lookup may take, 1..120
                        (default 10).
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
# `protection` asks about the destination, so its default is the repository's
# default branch (looked up below), not the checkout's.
if [ -z "$branch" ] && [ "$ACTION" != protection ]; then
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
    # maintenance pass without holding a conversation hook open.
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
  # Concurrent maintenance calls skip immediately instead of queueing behind
  # network I/O. The next scheduled pass can retry.
  oms_try_file_lock "$marker" ci_tick_locked || true
  exit 0
fi

if [ "$ACTION" = protection ]; then
  # Like history, this action has no ledger/journal path: it only reads GitHub.
  if ! command -v "$GH" >/dev/null 2>&1; then
    echo "ci-status protection: required check 'gate' unknown (gh CLI not installed)"
    exit 2
  fi
  protection_timeout="${OMS_CI_PROTECTION_TIMEOUT:-10}"
  case "$protection_timeout" in ""|*[!0-9]*) protection_timeout=10 ;; esac
  [ "$protection_timeout" -ge 1 ] && [ "$protection_timeout" -le 120 ] || protection_timeout=10
  OMS_PROTECTION_GH="$GH" OMS_PROTECTION_TIMEOUT="$protection_timeout" python3 - "$branch" <<'PY'
import json, os, subprocess, sys
from urllib.parse import quote

CHECK = "gate"
gh = os.environ["OMS_PROTECTION_GH"]
timeout = int(os.environ["OMS_PROTECTION_TIMEOUT"])


def clean(value, limit=120):
    text = "".join(ch if ch.isprintable() else "?" for ch in str(value))
    return text[:limit]


def get(path):
    """One GET. ("ok", body) for a readable answer, ("refused", message, status)
    for an API error body, ("unread", reason) for everything else. Only fixed
    reasons leave this function: no header, token or response body is printed."""
    try:
        done = subprocess.run(
            [gh, "api", "--method", "GET", "-H", "Accept: application/vnd.github+json", path],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            universal_newlines=True, errors="replace", timeout=timeout, check=False)
    except subprocess.TimeoutExpired:
        return ("unread", "timed out")
    except OSError:
        return ("unread", "gh could not run")
    try:
        body = json.loads(done.stdout)
    except (ValueError, RecursionError):
        body = None
    if done.returncode == 0:
        return ("ok", body) if body is not None else ("unread", "unreadable answer")
    if isinstance(body, dict) and isinstance(body.get("message"), str):
        status = str(body.get("status") or "")
        return ("refused", body["message"], status if status.isdigit() and len(status) == 3 else "?")
    return ("unread", "gh not authenticated or unreachable")


def items(value):
    return value if isinstance(value, list) else []


def unread(answer):
    if answer[0] == "refused":
        return "HTTP %s" % answer[2]
    return answer[1]


branch = sys.argv[1]
if not branch:
    repo = get("repos/{owner}/{repo}")
    name = repo[1].get("default_branch") if repo[0] == "ok" and isinstance(repo[1], dict) else None
    if not isinstance(name, str) or not name:
        reason = "unreadable answer" if repo[0] == "ok" else unread(repo)
        print("ci-status protection: required check '%s' unknown (default branch: %s)" % (CHECK, reason))
        raise SystemExit(2)
    branch = name
target = quote(branch, safe="/")

# Each source ends as "read" (it answered, and what it requires is known),
# "none" (it answered that nothing protects the branch) or a reason it could
# not be read. Only the first two can support "absent".
contexts = {"protection": set(), "rulesets": set()}
state = {}
reviews, dismiss, force_blocked, force_open = [], [], False, None

classic = get("repos/{owner}/{repo}/branches/%s/protection" % target)
if classic[0] == "ok" and isinstance(classic[1], dict):
    state["protection"] = "read"
    body = classic[1]
    checks = body.get("required_status_checks")
    if isinstance(checks, dict):
        for item in items(checks.get("contexts")):
            if isinstance(item, str):
                contexts["protection"].add(item)
        for item in items(checks.get("checks")):
            if isinstance(item, dict) and isinstance(item.get("context"), str):
                contexts["protection"].add(item["context"])
    review = body.get("required_pull_request_reviews")
    if isinstance(review, dict):
        count = review.get("required_approving_review_count")
        if type(count) is int and count > 0:
            reviews.append(count)
        dismiss.append(review.get("dismiss_stale_reviews") is True)
    force = body.get("allow_force_pushes")
    if isinstance(force, dict) and isinstance(force.get("enabled"), bool):
        force_blocked = force_blocked or not force["enabled"]
        force_open = force["enabled"]
elif classic[0] == "ok":
    state["protection"] = "unreadable answer"
elif classic[0] == "refused" and classic[1] == "Branch not protected":
    # Only a caller allowed to read protection gets this exact answer; a plain
    # 404 or a 403 cannot tell "not protected" from "not allowed to look".
    state["protection"] = "none"
    force_open = True
else:
    state["protection"] = unread(classic)

rules = get("repos/{owner}/{repo}/rules/branches/%s" % target)
if rules[0] == "ok" and isinstance(rules[1], list):
    state["rulesets"] = "read"
    for rule in rules[1]:
        if not isinstance(rule, dict):
            continue
        kind, parameters = rule.get("type"), rule.get("parameters")
        parameters = parameters if isinstance(parameters, dict) else {}
        if kind == "required_status_checks":
            for item in items(parameters.get("required_status_checks")):
                if isinstance(item, dict) and isinstance(item.get("context"), str):
                    contexts["rulesets"].add(item["context"])
        elif kind == "pull_request":
            count = parameters.get("required_approving_review_count")
            if type(count) is int and count > 0:
                reviews.append(count)
            dismiss.append(parameters.get("dismiss_stale_reviews_on_push") is True)
        elif kind == "non_fast_forward":
            force_blocked = True
elif rules[0] == "ok":
    state["rulesets"] = "unreadable answer"
else:
    state["rulesets"] = unread(rules)

labels = (("protection", "branch protection"), ("rulesets", "rulesets"))
conclusive = all(state[key] in ("read", "none") for key, _ in labels)
proof = [label for key, label in labels if CHECK in contexts[key]]
if proof and conclusive:
    verdict, code = "proven", 0
    detail = ", ".join(proof)
else:
    # A source that could not be read leaves the answer unknown even when the
    # other one requires the gate: unknown is never a pass.
    verdict, code = ("unknown", 2) if not conclusive else ("absent", 1)
    parts = []
    for key, label in labels:
        if state[key] == "read":
            parts.append("%s: %s" % (label, "requires it" if CHECK in contexts[key] else "does not require it"))
        elif state[key] == "none":
            parts.append("%s: not protected" % label)
        else:
            parts.append("%s: %s" % (label, state[key]))
    detail = "; ".join(parts)
print("ci-status protection [%s]: required check '%s' %s (%s)" % (clean(branch), CHECK, verdict, detail))

# The same answers carry three more facts. A value found in a source that was
# read stands; "none"/"no"/"allowed" needs every source read.
if reviews:
    print("  approving reviews required: %d" % max(reviews))
else:
    print("  approving reviews required: %s" % ("none" if conclusive else "unknown"))
if any(dismiss):
    print("  stale approvals dismissed: yes")
else:
    print("  stale approvals dismissed: %s" % ("no" if conclusive else "unknown"))
if force_blocked:
    print("  force pushes: blocked")
else:
    print("  force pushes: %s" % ("allowed" if conclusive and force_open is not None else "unknown"))
if verdict == "absent":
    print("  a workflow alone does not protect the branch: keep the full pre-push hook until '%s' is required" % CHECK)
elif verdict == "unknown":
    print("  unknown is not a pass: nothing here shows the branch is protected")
raise SystemExit(code)
PY
  exit $?
fi

if [ "$ACTION" = history ]; then
  command -v "$GH" >/dev/null 2>&1 || { echo "ci-status history: gh CLI not installed" >&2; exit 2; }
  [ -n "$branch" ] || branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo main)"
  # This action deliberately has no ledger/journal path: it only reads GitHub.
  HISTORY_JSON="$("$GH" run list --branch "$branch" --status completed --limit "$limit" \
    --json databaseId,headSha,conclusion,createdAt,updatedAt 2>/dev/null)" || {
    echo "ci-status history: gh could not list completed runs" >&2; exit 2;
  }
  OMS_HISTORY_JSON="$HISTORY_JSON" OMS_HISTORY_LIMIT="$limit" OMS_HISTORY_GH="$GH" python3 - "$branch" <<'PY'
import collections, datetime, json, os, re, subprocess, sys
branch = sys.argv[1]
try:
    runs = json.loads(os.environ.get("OMS_HISTORY_JSON", ""))
    if not isinstance(runs, list):
        raise ValueError("not a list")
except (ValueError, TypeError):
    print("ci-status history: gh returned an unreadable run list", file=sys.stderr)
    raise SystemExit(2)
limit = int(os.environ["OMS_HISTORY_LIMIT"])
runs = [r for r in runs if isinstance(r, dict) and r.get("status", "completed") == "completed"][:limit]
gh = os.environ["OMS_HISTORY_GH"]
job_runs, test_runs = collections.Counter(), collections.Counter()
for run in runs:
    sha = str(run.get("headSha") or "?")[:12]
    conclusion = run.get("conclusion") or "unknown"
    try:
        start = datetime.datetime.fromisoformat((run.get("createdAt") or "").replace("Z", "+00:00"))
        end = datetime.datetime.fromisoformat((run.get("updatedAt") or "").replace("Z", "+00:00"))
        minutes = max(0, int((end - start).total_seconds() // 60))
    except (ValueError, TypeError):
        minutes = 0
    print("%s  %s  %dm" % (sha, conclusion, minutes))
    if conclusion != "success" and run.get("databaseId") is not None:
        proc = subprocess.run([gh, "run", "view", str(run["databaseId"]), "--log-failed"],
                              stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                              universal_newlines=True, timeout=120, check=False)
        seen_jobs, seen_tests = set(), set()
        for line in proc.stdout.splitlines():
            # gh log-failed prefixes each record with the job name and tabs.
            if "\t" in line:
                name = line.split("\t", 1)[0].strip()
                if name:
                    seen_jobs.add(name)
            match = re.search(r"check:\s*([A-Za-z0-9_.-]+)\s+FAILED\b", line)
            if match:
                seen_tests.add(match.group(1))
        job_runs.update(seen_jobs)
        test_runs.update(seen_tests)
print("Failing jobs (runs):")
for name, count in sorted(job_runs.items(), key=lambda item: (-item[1], item[0])):
    print("  %s: %d" % (name, count))
print("Failing tests (runs):")
for name, count in sorted(test_runs.items(), key=lambda item: (-item[1], item[0])):
    suffix = " — quarantine candidate" if count >= 2 else ""
    print("  %s: %d%s" % (name, count, suffix))
PY
  exit $?
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
  --json status,conclusion,workflowName,headSha,url,attempt 2>/dev/null)" || gh_rc=$?
if [ "$gh_rc" -ne 0 ]; then
  echo "ci-status: gh not authenticated or unreachable (try: gh auth login)" >&2
  exit 2
fi
# Exit 0 with a body that is not a run list is a broken lookup, not "no runs":
# only a literal `[]` may read as an empty branch. Checked before any ledger or
# PR work so a corrupt answer is never recorded as a successful lookup.
shape="$(OMS_JSON="$json" python3 -c '
import json, os
try:
    ok = isinstance(json.loads(os.environ["OMS_JSON"]), list)
except ValueError:
    ok = False
print("ok" if ok else "bad")
' | tr -d '\r')"
if [ "$shape" != ok ]; then
  echo "ci-status: gh returned an unreadable run list (lookup failed)" >&2
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
runs = json.loads(raw)
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
    if r.get("attempt") is not None:
        row["attempt"] = r["attempt"]
    if r.get("workflowName"):
        row["workflow"] = r["workflowName"]
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
