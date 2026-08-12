#!/usr/bin/env bash
set -euo pipefail

# UserPromptSubmit hook: deterministic skill routing. Skills often
# go un-invoked because nothing at prompt time reminds the model they exist;
# this matches the prompt against the trigger phrases in skills.manifest.json
# and prints a one-line hint (stdout becomes injected context). Precision over
# recall: at most OMS_ROUTER_MAX suggestions per prompt, each skill suggested
# at most once per turn, silence on no match, and system-ish prompts
# (tool notifications, slash commands) are skipped entirely. Fail-open: this
# hook must never block a prompt.
#
# Automatic task recording is opt-in with OMS_AUTO_TASK=1. Disable the router
# entirely with OMS_SKILL_ROUTER_OFF=1. Claude Code installs this directly;
# Codex installs it through the repo-local oh-my-setting plugin.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/work-journal.sh
. "$ROOT/scripts/lib/work-journal.sh"

# Rollover/catch-up on each top-level agent prompt. This stays independent of
# skill routing (which users may disable): local materialization runs first,
# then at most one pending mirror item gets a two-second retry. On the first
# prompt of a local day it also prints a bounded journal digest — hook stdout
# becomes agent context, which is what makes the journal self-referencing.
if [ "${OMS_HARNESS_CHILD:-0}" != 1 ] &&
  git -C "${OMS_STATE_REPO:-$PWD}" rev-parse --git-dir >/dev/null 2>&1; then
  work_journal_prompt_tick "${OMS_STATE_REPO:-$PWD}"
fi

# Recover CI results at a natural start boundary without polling during work.
# Only GitHub-backed adopted repos participate; `tick` is locally deduped,
# throttled, two-second bounded by default, and fail-open.
ci_tick_repo="${OMS_STATE_REPO:-$PWD}"
if [ "${OMS_HARNESS_CHILD:-0}" != 1 ] && [ "${OMS_CI_TICK:-1}" = 1 ] &&
  [ -d "$ci_tick_repo/.oms" ] &&
  git -C "$ci_tick_repo" remote get-url origin 2>/dev/null | grep -Eq 'github\.com[:/]'; then
  (cd "$ci_tick_repo" && OMS_CI_TICK_QUIET=1 \
    bash "$ROOT/scripts/ci-status.sh" tick) >/dev/null 2>&1 || true
fi

# State-conditional hints: inject the one thing native skill matching cannot
# know — this repo's harness state. A stale claimed task outranks a repeated
# unresolved failure, which outranks an unforged lesson; one line, at most
# once per local day per repo, adopted repos only. The day marker lives under
# .oms/hooks/, the subtree already treated as live-session state.
# OMS_STATE_HINTS=0 disables. Fail-open.
state_hint() {
  local repo="$1"
  local day marker task failures skills
  day="$(date +%Y-%m-%d)"
  marker="$repo/.oms/hooks/state-hint.$day"
  [ -e "$marker" ] && return 0
  mkdir -p "$repo/.oms/hooks" 2>/dev/null || return 0
  : > "$marker" 2>/dev/null || return 0
  find "$repo/.oms/hooks" -maxdepth 1 -name 'state-hint.*' \
    ! -name "state-hint.$day" -delete 2>/dev/null || true
  task="$(cd "$repo" && bash "$ROOT/scripts/agent-task.sh" status --json 2>/dev/null || true)"
  failures="$(cd "$repo" && bash "$ROOT/scripts/fail-ledger.sh" list --json 2>/dev/null || true)"
  skills="$(find "$repo/.oms/skills" -mindepth 2 -maxdepth 2 -name SKILL.md 2>/dev/null | wc -l | tr -d ' ')"
  OMS_SH_TASK="$task" OMS_SH_FAILURES="$failures" OMS_SH_SKILLS="$skills" \
    OMS_SH_LEDGER="$repo/.oms/failures.jsonl" \
    OMS_SH_PLAN="$repo/.oms/plan/tasks.json" \
    OMS_SH_PROGRESS="$repo/.oms/plan/progress.jsonl" \
    OMS_SH_USAGE="$repo/.oms/usage.jsonl" \
    OMS_SH_SKILLS_DIR="$repo/.oms/skills" \
    OMS_SH_FAMILIES="$ROOT/scripts/lib/usage-families.json" \
    OMS_SH_USAGE_ROOTS="${OMS_USAGE_SKILL_ROOTS:-$HOME/.claude/skills:$HOME/.agents/skills}" \
    python3 - <<'PY' 2>/dev/null || true
import datetime, glob, json, os, re

def load(name):
    try:
        return json.loads(os.environ.get(name) or "{}")
    except ValueError:
        return {}

task = load("OMS_SH_TASK")
if task.get("present") and task.get("status") == "active" and task.get("stale"):
    print(
        "[oms] stale active task %s — read `oms agent-task status` before new"
        " work; the oms-agent-harness skill covers resume/handoff."
        % (task.get("task_id") or "?")
    )
    raise SystemExit(0)
# A parked goal outranks generic failure counts: the driver already digested
# its failure into one reason and one next command.
try:
    with open(os.environ.get("OMS_SH_PLAN", ""), encoding="utf-8") as fh:
        plan = json.load(fh)
except (OSError, ValueError):
    plan = {}
if plan.get("accept"):
    last_terminal = None
    try:
        with open(os.environ.get("OMS_SH_PROGRESS", ""), encoding="utf-8") as fh:
            for line in fh:
                try:
                    row = json.loads(line)
                except ValueError:
                    continue
                if row.get("kind") == "terminal":
                    last_terminal = row
    except OSError:
        pass
    if last_terminal and last_terminal.get("status") == "park":
        outer = os.path.join(os.path.dirname(os.environ.get("OMS_SH_PLAN", "")),
                             "autopilot-run.json")
        # The receipt itself is validated by autopilot status. Here it is only
        # a routing discriminator: direct the parent to the outer contract
        # without exposing recovery commands as an end-user procedure.
        if os.path.isfile(outer) and not os.path.islink(outer):
            print(
                "[oms] autopilot parked (%s) — parent agent: inspect and resume"
                " the validated autopilot receipt internally."
                % (last_terminal.get("reason") or "?")
            )
            raise SystemExit(0)
        print(
            "[oms] goal parked (%s) — parent agent: inspect the plan state and"
            " resume it internally."
            % (last_terminal.get("reason") or "?")
        )
        raise SystemExit(0)
failures = load("OMS_SH_FAILURES")
# Expired hook rows read as retired everywhere else; the daily nag must not
# count what `check` and `list` no longer hold against anyone.
rows = [f for f in failures.get("failures", [])
        if not f.get("resolved") and not f.get("expired")]
if len(rows) >= 2:
    print(
        "[oms] %d unresolved fail-ledger rows here — run `oms fail-ledger"
        " list` before retrying anything that already failed."
        % len(rows)
    )
    raise SystemExit(0)

# A failure that repeated and was then resolved is a learned lesson. Until the
# repo holds any project skill, offer the forge. The ledger is read raw
# because `list` zeroes a fingerprint's count on resolve, erasing exactly the
# repeated-then-resolved signal this hint keys on.
if os.environ.get("OMS_SH_SKILLS") == "0":
    state = {}
    try:
        with open(os.environ.get("OMS_SH_LEDGER", ""), encoding="utf-8") as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    row = json.loads(line)
                except ValueError:
                    continue
                fp = row.get("fingerprint")
                if not fp:
                    continue
                entry = state.setdefault(fp, {"count": 0, "resolved": False})
                if row.get("event") == "resolved" or row.get("resolved") is True:
                    entry["resolved"] = True
                elif row.get("event") == "fail":
                    entry["count"] += 1
                    entry["resolved"] = False
    except OSError:
        state = {}
    lessons = sum(1 for e in state.values() if e["count"] >= 2 and e["resolved"])
    if lessons:
        print(
            "[oms] %d repeated failure(s) here were resolved but no project"
            " skill records the lesson — `oms skill-forge add --name NAME`"
            " turns it into standing context." % lessons
        )
        raise SystemExit(0)

# Recurring successful usage with no covering skill is the signal the ledger
# can never see: a domain the sessions keep working in without standing
# context. Coverage matches each family's own pattern (plus its key) against
# project skills, so a forged skill silences its family whatever the agent
# named it; a linked global skill (covered_by) silences it machine-wide.
# Propose only — recurrence is not importance, the judgment stays with the
# agent. Rows past OMS_USAGE_TTL read as expired; gc compacts under the same
# predicate.
try:
    with open(os.environ.get("OMS_SH_FAMILIES", ""), encoding="utf-8") as fh:
        fam_table = json.load(fh).get("families", {})
except (OSError, ValueError):
    fam_table = {}
usage = {}
try:
    ttl = int(os.environ.get("OMS_USAGE_TTL") or 2592000)
except ValueError:
    ttl = 2592000
today = datetime.date.today()
try:
    with open(os.environ.get("OMS_SH_USAGE", ""), encoding="utf-8") as fh:
        for line in fh:
            try:
                row = json.loads(line)
            except ValueError:
                continue
            fam, day = row.get("family"), row.get("day")
            if not fam or fam not in fam_table:
                continue
            try:
                age = (today - datetime.date.fromisoformat(day)).days * 86400
            except (TypeError, ValueError):
                continue   # an unreadable day reads as expired
            if age > ttl or age < 0:
                continue
            counts = usage.setdefault(fam, {})
            try:
                counts[day] = counts.get(day, 0) + int(row.get("count") or 1)
            except (TypeError, ValueError):
                counts[day] = counts.get(day, 0) + 1
except OSError:
    usage = {}
if usage:
    min_hits = int(os.environ.get("OMS_USAGE_HINT_MIN") or 6)
    min_days = int(os.environ.get("OMS_USAGE_HINT_DAYS") or 2)
    roots = [r for r in (os.environ.get("OMS_SH_USAGE_ROOTS") or "").split(":") if r]
    skill_texts = []
    skills_dir = os.environ.get("OMS_SH_SKILLS_DIR") or ""
    for path in glob.glob(os.path.join(skills_dir, "*", "SKILL.md")):
        try:
            with open(path, encoding="utf-8", errors="replace") as fh:
                skill_texts.append(
                    (os.path.basename(os.path.dirname(path)), fh.read().lower())
                )
        except OSError:
            continue

    def covered(fam, spec):
        by = spec.get("covered_by")
        if by and any(os.path.isdir(os.path.join(r, by)) for r in roots):
            return True
        pattern = spec.get("pattern") or ""
        for name, text in skill_texts:
            if fam in name or fam in text:
                return True
            try:
                if pattern and re.search(pattern, text):
                    return True
            except re.error:
                continue
        return False

    for fam in sorted(usage):
        day_counts = usage[fam]
        total = sum(day_counts.values())
        if total < min_hits or len(day_counts) < min_days:
            continue
        if covered(fam, fam_table.get(fam) or {}):
            continue
        print(
            "[oms] '%s' tooling recurred %dx over %d day(s) with no covering"
            " skill — if a real lesson repeats with it, `oms skill-forge add`"
            " gives future sessions a warm start (recurrence isn't"
            " importance; your judgment)." % (fam, total, len(day_counts))
        )
        break
PY
}

if [ "${OMS_HARNESS_CHILD:-0}" != 1 ] && [ "${OMS_STATE_HINTS:-1}" = "1" ] &&
  [ -d "${OMS_STATE_REPO:-$PWD}/.oms" ]; then
  state_hint "${OMS_STATE_REPO:-$PWD}" || true
fi

[ "${OMS_SKILL_ROUTER_OFF:-0}" = "1" ] && exit 0

MANIFEST="${OMS_SKILL_MANIFEST:-$ROOT/skills.manifest.json}"
HELPER="$ROOT/scripts/lib/hook_state.py"
[ -f "$MANIFEST" ] || exit 0
[ -f "$HELPER" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

# The helper owns routing state and fail-opens on malformed hook payloads.
OMS_HOOK_PAYLOAD="$(cat)" python3 "$HELPER" route --manifest "$MANIFEST" || exit 0
