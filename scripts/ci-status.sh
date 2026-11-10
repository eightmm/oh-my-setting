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
# throttles pending lookups instead of polling in the foreground. `flaky` reads
# that ledger only and lists the commits recorded red and then green. `protection`
# answers the question a quick push hook rests on: is CI's `gate` job a required
# check on the destination branch? It only reads GitHub, runs only when asked,
# and has three answers — a lookup that could not read a source is unknown,
# never absent and never a pass. `triage` reads why HEAD's run is red: the
# failing jobs, a bounded scanned tail of each one's log, and whether the same
# job is also red on the base branch. It prints a brief and changes nothing.

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
triage_sha=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    status) ACTION="status"; shift ;;
    record|--record) ACTION="record"; shift ;;
    tick) ACTION="tick"; shift ;;
    history) ACTION="history"; shift ;;
    flaky) ACTION="flaky"; shift ;;
    protection) ACTION="protection"; shift ;;
    triage) ACTION="triage"; shift ;;
    --limit)
      [ "$#" -ge 2 ] || { echo "error: --limit needs a positive integer" >&2; exit 2; }
      limit="$2"
      case "$limit" in *[!0-9]*|"") echo "error: --limit must be a positive integer" >&2; exit 2 ;; esac
      [ "$limit" -gt 0 ] || { echo "error: --limit must be a positive integer" >&2; exit 2; }
      shift 2
      ;;
    --sha)
      [ "$#" -ge 2 ] || { echo "error: --sha needs a commit id" >&2; exit 2; }
      triage_sha="$2"
      case "$triage_sha" in
        *[!0-9a-fA-F]*|"") echo "error: --sha must be 7 to 40 hexadecimal characters" >&2; exit 2 ;;
      esac
      [ "${#triage_sha}" -ge 7 ] && [ "${#triage_sha}" -le 40 ] ||
        { echo "error: --sha must be 7 to 40 hexadecimal characters" >&2; exit 2; }
      shift 2
      ;;
    -h|--help)
      cat <<'EOF'
Usage: ci-status.sh [status|record|tick] [branch]
       ci-status.sh history [--limit N] [branch]
       ci-status.sh protection [branch]
       ci-status.sh triage [--sha COMMIT] [branch]
       ci-status.sh flaky [branch]

status  Print the latest CI run and fail on a completed red result (default).
record  Also append the result to .oms/ci.jsonl and Work Journal. A completed
        red run's row also names its failing jobs and checks when gh can
        list them (not from tick, which has no time for the log).
tick    Fail-open bounded refresh for prompt/session boundaries. A completed
        current SHA and a recent pending lookup are local no-ops.
history Show recent completed runs and ranked failing jobs/tests (read-only).
flaky   List commits whose recorded CI went red and later green without a
        new commit, with the jobs and checks the red run named. Reads
        .oms/ci.jsonl only; an observation, not a verdict, and nothing is
        skipped, quarantined or re-run.
protection
        Report whether CI job 'gate' is a required check on the branch
        (default: the repository's default branch), from branch protection
        and active rulesets (read-only GET requests; nothing is changed).
        Exit 0 proven, 1 absent, 2 unknown: a source that could not be read
        (no gh, no permission, timeout) is unknown, never absent or a pass.
triage  Explain a red run for the branch's HEAD, or for --sha COMMIT on that
        branch (read-only): the failing
        jobs and steps, a bounded tail of each failing job's log (withheld
        when it looks sensitive), whether the same job is also red on the
        base branch, and a brief to hand to whoever fixes it. Nothing is
        re-run, retried, pushed, dispatched or recorded. Exit 0 nothing red,
        1 a red run was triaged, 2 the run could not be read.

Environment:
  OMS_CI_TICK_INTERVAL  Minimum seconds between unresolved lookups (default 120).
  OMS_CI_TICK_TIMEOUT   Network subprocess budget in seconds (default 2).
  OMS_CI_PROTECTION_TIMEOUT
                        Seconds each protection lookup may take, 1..120
                        (default 10).
  OMS_CI_TRIAGE_TIMEOUT Seconds the failed-log download may take, 1..300
                        (default 60); each other triage lookup gets at most 20.
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
if [ -n "$triage_sha" ] && [ "$ACTION" != triage ]; then
  echo "error: --sha is only read by triage" >&2
  exit 2
fi
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
        # The failed-log lookup does not fit a hook's budget; an explicit record adds the names.
        env=dict(os.environ, OMS_CI_RECORD_NAMES="0"),
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

if [ "$ACTION" = flaky ]; then
  # Ledger only: no gh, no network, no write. A commit recorded red and later
  # green is what was observed; whether the test is flaky or the first run hit
  # a broken runner is for the reader, so nothing is skipped or quarantined.
  state_root="$(oms_repo_root "$PWD")"
  python3 - "${OMS_CI_LEDGER:-$state_root/.oms/ci.jsonl}" "$branch" <<'PY'
import collections, json, sys
ledger, branch = sys.argv[1:]
RED = {"failure", "timed_out", "startup_failure"}
rows = []
try:
    with open(ledger, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if isinstance(row, dict) and row.get("branch") == branch and row.get("status") == "completed":
                rows.append(row)
except OSError:
    print("ci-status flaky [%s]: no recorded CI results (run: oms ci-status record)" % branch)
    raise SystemExit(0)
red, observed = {}, []
for row in rows:
    sha, conclusion = row.get("sha"), row.get("conclusion")
    if not isinstance(sha, str) or not sha:
        continue
    if conclusion in RED:
        # The newest red row wins: it is the one a later record completed with names.
        red[sha] = row
    elif conclusion == "success" and sha in red:
        observed.append(red.pop(sha))
jobs, checks = collections.Counter(), collections.Counter()
print("ci-status flaky [%s]: %d commit(s) recorded red, then green without a new commit, in %d recorded result(s)" % (
    branch, len(observed), len(rows)))
for row in observed:
    failed_jobs = [n for n in (row.get("failed_jobs") or []) if isinstance(n, str)]
    failed_checks = [n for n in (row.get("failed_checks") or []) if isinstance(n, str)]
    jobs.update(failed_jobs)
    checks.update(failed_checks)
    named = failed_jobs + failed_checks
    print("  %s  %s  %s" % (str(row.get("sha"))[:12], row.get("ts", "?"),
                            ", ".join(named) if named else "failing names not recorded"))
for title, counter in (("jobs", jobs), ("checks", checks)):
    if counter:
        print("flaky_observed %s:" % title)
        for name, count in sorted(counter.items(), key=lambda item: (-item[1], item[0])):
            print("  %s: %d" % (name, count))
if observed:
    print("observed only: a rerun can also pass because a runner recovered; nothing was skipped, quarantined or re-run")
PY
  exit $?
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

if [ "$ACTION" = triage ]; then
  # Like history and protection: no ledger, no journal, only reads of GitHub.
  if ! command -v "$GH" >/dev/null 2>&1; then
    echo "ci-status triage: unknown (gh CLI not installed)"
    exit 2
  fi
  triage_timeout="${OMS_CI_TRIAGE_TIMEOUT:-60}"
  case "$triage_timeout" in ""|*[!0-9]*) triage_timeout=60 ;; esac
  [ "$triage_timeout" -ge 1 ] && [ "$triage_timeout" -le 300 ] || triage_timeout=60
  triage_head="$(git rev-parse HEAD 2>/dev/null || true)"
  [ "$branch" = "$(git rev-parse --abbrev-ref HEAD 2>/dev/null || true)" ] || triage_head=""
  [ -z "$triage_sha" ] || triage_head="$(printf '%s' "$triage_sha" | tr 'A-F' 'a-f')"
  triage_dir="$(mktemp -d)" || exit 2
  trap 'rm -rf "$triage_dir"' EXIT
  triage_rc=0
  OMS_TRIAGE_GH="$GH" OMS_TRIAGE_TIMEOUT="$triage_timeout" OMS_TRIAGE_HEAD="$triage_head" \
    OMS_TRIAGE_DIR="$triage_dir" OMS_TRIAGE_COMMON="$ROOT/scripts/lib/agent-memory-common.sh" \
    python3 - "$branch" <<'PY' || triage_rc=$?
import json, os, re, subprocess, sys

gh = os.environ["OMS_TRIAGE_GH"]
log_timeout = int(os.environ["OMS_TRIAGE_TIMEOUT"])
timeout = min(log_timeout, 20)
head = os.environ.get("OMS_TRIAGE_HEAD", "")
work = os.environ["OMS_TRIAGE_DIR"]
branch = sys.argv[1]
RED = ("failure", "timed_out", "cancelled", "startup_failure")
MAX_JOBS, TAIL_LINES, LINE_CHARS, LOG_BYTES = 6, 25, 200, 2 * 1024 * 1024
ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")
STAMP = re.compile(r"^﻿?\d{4}-\d{2}-\d{2}T[0-9:.]+Z ?")


def clean(value, limit=120):
    text = "".join(ch if ch.isprintable() else "?" for ch in str(value))
    return text[:limit]


def unknown(reason):
    print("ci-status triage [%s]: unknown (%s)" % (clean(branch), reason))
    print("  unknown is not a pass: nothing here shows the run is green")
    raise SystemExit(2)


def ask(args, limit=timeout, cap=None):
    """stdout of one read-only gh call, or None. Nothing gh wrote to stderr is kept."""
    try:
        proc = subprocess.Popen([gh] + args, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                stderr=subprocess.DEVNULL)
    except OSError:
        return None
    try:
        out = proc.communicate(timeout=limit)[0]
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.communicate()
        return None
    if proc.returncode != 0:
        return None
    if cap is not None and len(out) > cap:
        out = out[-cap:]  # the end of a log is where a job says why it stopped
    return out.decode("utf-8", "replace")


def parsed(args):
    out = ask(args)
    try:
        return json.loads(out) if out is not None else None
    except (ValueError, RecursionError):
        return None


def listing(args):
    rows = parsed(args)
    return [r for r in rows if isinstance(r, dict)] if isinstance(rows, list) else None


def jobs_of(run_id):
    body = parsed(["run", "view", str(run_id), "--json", "jobs"])
    jobs = body.get("jobs") if isinstance(body, dict) else None
    if not isinstance(jobs, list):
        return None
    return [j for j in jobs if isinstance(j, dict) and isinstance(j.get("name"), str)]


FIELDS = "databaseId,status,conclusion,workflowName,headSha,url"
runs = listing(["run", "list", "--branch", branch, "--limit", "20", "--json", FIELDS])
if runs is None:
    unknown("run list: gh not authenticated, unreachable or unreadable")
done = [r for r in runs if r.get("status") == "completed"]
def is_head(row):
    return bool(head) and isinstance(row.get("headSha"), str) and row["headSha"].startswith(head)


mine = [r for r in done if is_head(r)]
if head and not mine:
    pending = any(is_head(r) for r in runs)
    unknown("%s has %s" % (head[:12], "a run that has not finished" if pending
                              else "no run among the branch's newest %d" % len(runs)))
pool = mine or done
if not pool:
    unknown("no completed run on this branch")
sha = pool[0].get("headSha")
pool = [r for r in pool if r.get("headSha") == sha]
red = [r for r in pool if r.get("conclusion") in RED]
if not red:
    print("ci-status triage [%s]: nothing red on %s (%d completed run%s)" % (
        clean(branch), clean(sha or "?", 12), len(pool), "" if len(pool) == 1 else "s"))
    raise SystemExit(0)
run = red[0]
run_id = run.get("databaseId")
if type(run_id) is not int:
    unknown("run id unreadable")
jobs = jobs_of(run_id)
if jobs is None:
    unknown("job list unreadable")
failing = [j for j in jobs if j.get("conclusion") in RED]

# The base: the default branch, or for the default branch itself the newest earlier commit's run.
base_note, base_jobs = None, None
repo = parsed(["api", "--method", "GET", "repos/{owner}/{repo}"])
default = repo.get("default_branch") if isinstance(repo, dict) else None
if not isinstance(default, str) or not default:
    base_note = "base branch unreadable"
else:
    if default == branch:
        # The base of the default branch is its own previous commit: the rows listed after this run.
        rows = runs[runs.index(run) + 1:]
    else:
        rows = listing(["run", "list", "--branch", default, "--limit", "20", "--json", FIELDS])
    base = None
    for row in rows or []:
        if (row.get("status") == "completed" and row.get("headSha") != sha
                and row.get("workflowName") == run.get("workflowName")):
            base = row
            break
    if rows is None:
        base_note = "base runs unreadable"
    elif base is None or type(base.get("databaseId")) is not int:
        base_note = "no completed %s run on %s to compare" % (
            clean(run.get("workflowName") or "workflow", 40), clean(default, 40))
    else:
        listed = jobs_of(base["databaseId"])
        if listed is None:
            base_note = "base job list unreadable"
        else:
            base_jobs = {j["name"]: j.get("conclusion") for j in listed}
            base_note = "%s %s" % (clean(default, 40), clean(base.get("headSha") or "?", 12))


def against_base(name):
    if base_jobs is None:
        return "unknown"
    if name not in base_jobs:
        return "unknown (job not in the base run)"
    return "also red on base (preexisting)" if base_jobs[name] in RED else "green on base (regression candidate)"


# Failed-step log tails, one file per job, scanned before a byte is printed.
tails, checks = {}, {}
log_state = "none"
if failing:
    log = ask(["run", "view", str(run_id), "--log-failed"], limit=log_timeout, cap=LOG_BYTES)
    log_state = "read" if log is not None else "unread"
    for line in (log or "").splitlines():
        parts = line.split("\t", 2)
        if len(parts) < 3:
            continue
        text = STAMP.sub("", ANSI.sub("", parts[2])).rstrip()
        if not text:
            continue
        found = re.search(r"check:\s*([A-Za-z0-9_.-]+)\s+FAILED\b", text)
        if found:
            checks.setdefault(parts[0].strip(), []).append(found.group(1))
        tails.setdefault(parts[0].strip(), []).append(clean(text, LINE_CHARS))
files = {}
for index, job in enumerate(failing[:MAX_JOBS]):
    lines = tails.get(job["name"].strip(), [])[-TAIL_LINES:]
    if lines:
        path = os.path.join(work, "job-%d.txt" % index)
        with open(path, "w", encoding="utf-8") as handle:
            handle.write("\n".join(lines) + "\n")
        files[index] = (path, lines)
withheld, scanned = set(), not files
if files:
    script = ('. "$1" || exit 3; shift; for f in "$@"; do '
              'if agent_memory_file_has_sensitive_content "$f"; then printf "%s\\n" "$f"; fi; done')
    try:
        scan = subprocess.run(["bash", "-c", script, "_", os.environ["OMS_TRIAGE_COMMON"]]
                              + [path for path, _ in files.values()], stdin=subprocess.DEVNULL,
                              stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, universal_newlines=True,
                              timeout=30, check=False)
        scanned = scan.returncode == 0
        withheld = set(scan.stdout.split("\n"))
    except (OSError, subprocess.SubprocessError):
        scanned = False

print("ci-status triage [%s]: %s on %s: %d failing job%s%s  %s" % (
    clean(branch), clean(run.get("conclusion")), clean(sha or "?", 12), len(failing),
    "" if len(failing) == 1 else "s",
    "" if failing else " listed (the run failed before or outside its jobs)", clean(run.get("url") or "", 200)))
print("  compared with: %s" % (base_note if base_jobs is not None else "unknown (%s)" % base_note))
for index, job in enumerate(failing[:MAX_JOBS]):
    name = job["name"]
    print("- job: %s" % clean(name))
    print("  base: %s" % against_base(name))
    steps = [clean(s.get("name"), 80) for s in (job.get("steps") if isinstance(job.get("steps"), list) else [])
             if isinstance(s, dict) and s.get("conclusion") in RED and isinstance(s.get("name"), str)]
    if steps:
        print("  failed step%s: %s" % ("" if len(steps) == 1 else "s", "; ".join(steps[:5])))
    named = sorted(set(checks.get(name.strip(), [])))
    if named:
        print("  failed checks: %s%s" % (", ".join(named[:12]),
                                         " (+%d more)" % (len(named) - 12) if len(named) > 12 else ""))
    if index not in files:
        print("  log: %s" % ("not downloaded (gh could not read it in time)" if log_state == "unread"
                             else "no failed-step lines for this job"))
    elif not scanned:
        print("  log: withheld (the sensitive-content scan could not run)")
    elif files[index][0] in withheld:
        print("  log: withheld (sensitive-looking content)")
    else:
        print("  log tail (last %d line%s):" % (len(files[index][1]), "" if len(files[index][1]) == 1 else "s"))
        for line in files[index][1]:
            print("    | " + line)
if len(failing) > MAX_JOBS:
    print("- %d more failing job(s) not shown" % (len(failing) - MAX_JOBS))
print("brief: fix a job marked regression candidate first; a job that is also red on base was not")
print("  broken by this branch. Reproduce the failed check locally before changing code. This is an")
print("  observation of CI, not verification evidence; nothing was re-run, retried, pushed or recorded.")
raise SystemExit(1)
PY
  exit "$triage_rc"
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
  --json status,conclusion,workflowName,headSha,url,attempt,databaseId 2>/dev/null)" || gh_rc=$?
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
  OMS_CI_HEAD="$HEAD_SHA" OMS_CI_AHEAD="$AHEAD" OMS_CI_GH="$GH" \
  OMS_CI_TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)" python3 - <<'PY' || ci_rc=$?
import json, os, re, subprocess, sys
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
    journal = dict(row)  # the Work Journal row keeps its recorded shape
    # A red run's row names what failed, so a later green run of the same
    # commit can be read as "this job went red, then green" (ci-status flaky).
    # Names only, bounded, and only when gh lists them in time; never the log.
    if (status == "completed" and concl in ("failure", "timed_out", "startup_failure")
            and os.environ.get("OMS_CI_RECORD_NAMES", "1") != "0" and r.get("databaseId") is not None):
        try:
            proc = subprocess.run([os.environ["OMS_CI_GH"], "run", "view", str(r["databaseId"]), "--log-failed"],
                                  stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                  universal_newlines=True, errors="replace", timeout=30, check=False)
            text = proc.stdout if proc.returncode == 0 else ""
        except (OSError, KeyError, subprocess.SubprocessError):
            text = ""
        jobs, checks = set(), set()
        for line in text.splitlines():
            if "\t" in line:
                name = line.split("\t", 1)[0].strip()
                if re.fullmatch(r"[A-Za-z0-9_. ()/,:-]{1,80}", name):
                    jobs.add(name)
            found = re.search(r"check:\s*([A-Za-z0-9_.-]{1,80})\s+FAILED\b", line)
            if found:
                checks.add(found.group(1))
        if jobs:
            row["failed_jobs"] = sorted(jobs)[:20]
        if checks:
            row["failed_checks"] = sorted(checks)[:20]
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
    same = bool(prev) and prev.get("sha") == sha and prev.get("conclusion") == concl
    # A row tick wrote without names is completed by the first record that has them.
    named = same and bool(row.get("failed_jobs") or row.get("failed_checks")) and not (
        prev.get("failed_jobs") or prev.get("failed_checks"))
    if not same or named:
        with open(ledger, "a", encoding="utf-8") as f:
            f.write(json.dumps(row, ensure_ascii=False) + "\n")
    journal_row = os.environ.get("OMS_CI_JOURNAL_ROW", "")
    if journal_row and status == "completed":
        with open(journal_row, "w", encoding="utf-8", newline="\n") as f:
            f.write(json.dumps(journal, ensure_ascii=False) + "\n")

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
