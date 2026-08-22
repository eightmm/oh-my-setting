#!/usr/bin/env bash
set -euo pipefail

# PROJECT.md -> proposed plan tasks. Decomposition is a model judgment, so by
# design this PROPOSES only: the task list becomes agent-plan state exclusively
# through an explicit --apply of a reviewed proposal file. That keeps the
# cross-family review verdict intact — generated plans enter the board through
# approval, never silently. The chain it completes:
#   PROJECT.md -> plan-from-spec (propose) -> [review] -> --apply -> goal-drive.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"

REPO="$PWD"
PROVIDER="codex"
APPLY_FILE=""
VALIDATE_SPEC=""
MAX_TASKS=6
ID_PREFIX=""
ALLOWED_ENVELOPE=""
EXPECTED_PROPOSAL_SHA=""
MODEL=""
FALLBACK_MODEL=""
REASONING_EFFORT=auto
PROVIDER_TIMEOUT="${OMS_PEER_TIMEOUT:-5m}"

usage() {
  cat <<'EOF'
Usage: plan-from-spec.sh [--repo PATH] [--to PROVIDER] [--max-tasks N]
                         [--id-prefix PREFIX] [--allowed PATHS]
                         [--model MODEL] [--fallback-model MODEL]
                         [--reasoning-effort E] [--provider-timeout DUR]
       plan-from-spec.sh [--repo PATH] --apply PROPOSAL.json

Read the repository's PROJECT.md contract (Goal/Scope/Non-goals, Commands,
Verification) and ask the selected peer to decompose the remaining work into
plan tasks. The result is written as a PROPOSAL under .oms/plan/ and printed
for review — nothing touches the task board until --apply.

  --repo PATH     Repository with a PROJECT.md (default: current directory).
  --to PROVIDER   Peer for the decomposition call (default: codex).
  --max-tasks N   Cap on proposed tasks (default 6, max 12).
  --id-prefix P   Require every generated task id to start with P. Bounded
                  autopilot replans use r1- so a second tranche is observable.
  --allowed PATHS Comma-separated immutable path envelope for every proposed
                  task. Stored in the proposal and rechecked during apply.
  --validate-spec FILE  Preflight a spec candidate's acceptance contract
                   (same parser as propose; state is not checked — a
                   candidate is draft by design). Used by intent adopt.
  --model MODEL    Exact planner model forwarded to agent-call.
  --fallback-model MODEL
                  One-shot planner capacity fallback model.
  --reasoning-effort E
                  auto, low, medium, high, xhigh, max, or ultra.
  --provider-timeout DUR
                  Planner wall-clock timeout (for example 15m).
  --apply FILE    Append a reviewed proposal's tasks to the plan. Creates the
                  plan (goal + acceptance from PROJECT.md) when absent; an
                  existing plan and its acceptance command are never replaced.
  --expected-proposal-sha256 SHA
                  With --apply: the digest of the exact reviewed proposal
                  bytes. Apply fails when the file no longer matches, binding
                  the parent's review to what actually executes.

A PROJECT.md still in `- State: draft` is refused: decomposing an unconfirmed
spec automates guessing.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --to) [ "$#" -ge 2 ] || fail "--to requires a provider"; PROVIDER="$2"; shift 2 ;;
    --max-tasks)
      [ "$#" -ge 2 ] || fail "--max-tasks requires a count"
      case "$2" in *[!0-9]*|"") fail "--max-tasks requires a positive integer" ;; esac
      [ "$2" -ge 1 ] && [ "$2" -le 12 ] || fail "--max-tasks must be 1..12"
      MAX_TASKS="$2"; shift 2 ;;
    --id-prefix)
      [ "$#" -ge 2 ] || fail "--id-prefix requires a value"
      ID_PREFIX="$2"; shift 2 ;;
    --allowed)
      [ "$#" -ge 2 ] || fail "--allowed requires paths"
      ALLOWED_ENVELOPE="$2"; shift 2 ;;
    --model)
      [ "$#" -ge 2 ] || fail "--model requires a value"
      MODEL="$2"; shift 2 ;;
    --fallback-model)
      [ "$#" -ge 2 ] || fail "--fallback-model requires a value"
      FALLBACK_MODEL="$2"; shift 2 ;;
    --reasoning-effort)
      [ "$#" -ge 2 ] || fail "--reasoning-effort requires a value"
      REASONING_EFFORT="$2"; shift 2 ;;
    --provider-timeout)
      [ "$#" -ge 2 ] || fail "--provider-timeout requires a duration"
      PROVIDER_TIMEOUT="$2"; shift 2 ;;
    --apply) [ "$#" -ge 2 ] || fail "--apply requires a file"; APPLY_FILE="$2"; shift 2 ;;
    --validate-spec)
      [ "$#" -ge 2 ] || fail "--validate-spec requires a spec file"
      VALIDATE_SPEC="$2"; shift 2 ;;
    --expected-proposal-sha256)
      [ "$#" -ge 2 ] || fail "--expected-proposal-sha256 requires a digest"
      EXPECTED_PROPOSAL_SHA="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

case "$REASONING_EFFORT" in
  auto|low|medium|high|xhigh|max|ultra) ;;
  *) fail "--reasoning-effort must be auto, low, medium, high, xhigh, max, or ultra" ;;
esac
printf '%s\n' "$PROVIDER_TIMEOUT" | grep -Eq '^[1-9][0-9]*([smh])?$' ||
  fail "--provider-timeout must be a positive duration such as 15m"
python3 - "$PROVIDER_TIMEOUT" <<'PY' ||
import re, sys
match = re.fullmatch(r"([1-9][0-9]*)(s|m|h)?", sys.argv[1])
if not match:
    raise SystemExit(1)
value = int(match.group(1))
unit = match.group(2) or "s"
milliseconds = value * {"s": 1000, "m": 60000, "h": 3600000}[unit]
raise SystemExit(0 if milliseconds <= 24 * 60 * 60 * 1000 else 1)
PY
  fail "--provider-timeout must not exceed 24h"

REPO="$(oms_repo_root "$REPO")" || fail "bad --repo"
SPEC="$REPO/PROJECT.md"
PLAN_DIR="$REPO/.oms/plan"
PLAN_FILE="$PLAN_DIR/tasks.json"

[ "${OMS_HARNESS_CHILD:-0}" != 1 ] ||
  fail "plan-from-spec is parent-only; a harness child cannot propose or apply plan topology"

case "$ID_PREFIX" in
  "") ;;
  *[!A-Za-z0-9._-]*) fail "--id-prefix must match [A-Za-z0-9._-]+" ;;
esac

if [ -n "$EXPECTED_PROPOSAL_SHA" ]; then
  [ -n "$APPLY_FILE" ] || fail "--expected-proposal-sha256 requires --apply"
  case "$EXPECTED_PROPOSAL_SHA" in
    *[!0-9a-f]*|"") fail "--expected-proposal-sha256 must be a lowercase SHA-256" ;;
  esac
  [ "${#EXPECTED_PROPOSAL_SHA}" -eq 64 ] ||
    fail "--expected-proposal-sha256 must be a lowercase SHA-256"
fi

# Shared validator: a proposal is usable only when every task is mechanically
# safe to hand to plan-run (id shape, non-empty scope, non-empty verify,
# dependencies that stay inside the proposal).
validate_proposal() {  # FILE [APPLY] -> prints "ok <count>" or fails with reason
  python3 - "$1" "$MAX_TASKS" "$ID_PREFIX" "$ALLOWED_ENVELOPE" "$PLAN_FILE" "${2:-0}" <<'PY'
import json, re, sys, unicodedata

try:
    data = json.load(open(sys.argv[1], encoding="utf-8"))
except (OSError, ValueError) as exc:
    sys.stderr.write("proposal is not valid JSON: %s\n" % exc)
    sys.exit(3)
top_keys = {
    "schema", "kind", "spec_sha256", "plan_sha256", "base_sha",
    "id_prefix", "allowed_envelope", "acceptance_files", "tasks",
}
if (not isinstance(data, dict) or set(data) != top_keys or data.get("schema") != 1 or
        data.get("kind") != "agent-plan-proposal"):
    sys.stderr.write("proposal does not match the exact reviewed schema\n")
    sys.exit(3)
tasks = data.get("tasks")
if not isinstance(tasks, list) or not tasks:
    sys.stderr.write("proposal carries no tasks\n")
    sys.exit(3)
if len(tasks) > int(sys.argv[2]):
    sys.stderr.write("proposal exceeds the task cap (%d)\n" % int(sys.argv[2]))
    sys.exit(3)
acceptance_files = data.get("acceptance_files")
if (not isinstance(acceptance_files, list) or
        any(not isinstance(item, str) or not item for item in acceptance_files) or
        len(acceptance_files) > 64 or
        any(len(item.encode("utf-8")) > 240 for item in acceptance_files) or
        acceptance_files != sorted(set(acceptance_files))):
    sys.stderr.write("proposal acceptance_files must be a sorted unique path list (max 64 paths, 240 bytes each)\n")
    sys.exit(3)
id_re = re.compile(r"^[A-Za-z0-9._-]+$")
prefix = sys.argv[3]
envelope_text = sys.argv[4]
plan_path = sys.argv[5]
apply_mode = sys.argv[6] == "1"

def reject_controls(value, label):
    if any(unicodedata.category(ch) in ("Cc", "Cf", "Cs") for ch in value):
        raise ValueError("%s contains a control or format character" % label)

def clean_rel(value, label):
    if not isinstance(value, str):
        raise ValueError("%s must be a string" % label)
    reject_controls(value, label)
    value = value.strip().replace("\\", "/")
    while value.startswith("./"):
        value = value[2:]
    value = value.rstrip("/") or "."
    if (value.startswith("/") or re.match(r"^[A-Za-z]:", value) or
            (value != "." and any(part in ("", ".", "..") for part in value.split("/")))):
        raise ValueError("%s must be a normalized repo-relative path" % label)
    return value

try:
    envelope = [clean_rel(item, "allowed envelope")
                for item in re.split(r"[,\s]+", envelope_text) if item.strip()]
    normalized_acceptance = [clean_rel(item, "acceptance file") for item in acceptance_files]
    if normalized_acceptance != acceptance_files or any(item == "." for item in normalized_acceptance):
        raise ValueError("proposal acceptance_files must be normalized repo-relative file paths")
except ValueError as exc:
    sys.stderr.write(str(exc) + "\n"); sys.exit(3)

existing = {}
try:
    with open(plan_path, encoding="utf-8") as handle:
        value = json.load(handle)
    if isinstance(value, dict) and isinstance(value.get("tasks"), dict):
        existing = value["tasks"]
except OSError:
    pass
except ValueError:
    sys.stderr.write("existing plan is invalid JSON\n"); sys.exit(3)

seen = []
for t in tasks:
    if not isinstance(t, dict):
        sys.stderr.write("task entries must be objects\n"); sys.exit(3)
    if set(t) != {"id", "title", "allowed", "verify", "depends"}:
        sys.stderr.write("task entries must match id/title/allowed/verify/depends exactly\n"); sys.exit(3)
    tid = t.get("id") or ""
    if not id_re.fullmatch(tid):
        sys.stderr.write("bad task id: %r\n" % tid); sys.exit(3)
    if prefix and not tid.startswith(prefix):
        sys.stderr.write("task id %s does not start with required prefix %s\n" % (tid, prefix)); sys.exit(3)
    if tid in seen:
        sys.stderr.write("duplicate task id: %s\n" % tid); sys.exit(3)
    # A replay of the exact proposal after an interrupted parent run is
    # resolved atomically by agent-plan, which compares every immutable task
    # field. Fresh generation still rejects duplicate ids here.
    if tid in existing and not apply_mode:
        sys.stderr.write("task id already exists in the plan: %s\n" % tid); sys.exit(3)
    if not isinstance(t.get("title"), str) or not t["title"].strip():
        sys.stderr.write("task %s has no title\n" % tid); sys.exit(3)
    try:
        reject_controls(t["title"], "task %s title" % tid)
    except ValueError as exc:
        sys.stderr.write(str(exc) + "\n"); sys.exit(3)
    allowed = t.get("allowed")
    if (not isinstance(allowed, list) or not allowed or
            any(not isinstance(a, str) or not a.strip() for a in allowed)):
        sys.stderr.write("task %s has empty allowed paths\n" % tid); sys.exit(3)
    if any(a.startswith("/") or ".." in a for a in allowed):
        sys.stderr.write("task %s allowed paths must be repo-relative\n" % tid); sys.exit(3)
    if envelope:
        try:
            cleaned = [clean_rel(a, "task allowed path") for a in allowed]
        except ValueError as exc:
            sys.stderr.write(str(exc) + "\n"); sys.exit(3)
        for candidate in cleaned:
            if not any(root == "." or candidate == root or candidate.startswith(root + "/")
                       for root in envelope):
                sys.stderr.write("task %s widens the allowed path envelope\n" % tid); sys.exit(3)
    if not isinstance(t.get("verify"), str) or not t["verify"].strip():
        sys.stderr.write("task %s has no verify command\n" % tid); sys.exit(3)
    try:
        reject_controls(t["verify"], "task %s verify" % tid)
    except ValueError as exc:
        sys.stderr.write(str(exc) + "\n"); sys.exit(3)
    depends = t.get("depends")
    if (not isinstance(depends, list) or any(not isinstance(dep, str) for dep in depends)
            or any(not id_re.fullmatch(dep) for dep in depends)
            or len(depends) != len(set(depends))):
        sys.stderr.write("task %s dependencies must be unique ids\n" % tid); sys.exit(3)
    for dep in depends:
        if dep in seen:
            continue
        if dep not in existing:
            sys.stderr.write("task %s depends on unknown/later task %s\n" % (tid, dep)); sys.exit(3)
        if existing[dep].get("state") != "done":
            sys.stderr.write("task %s depends on unfinished existing task %s\n" % (tid, dep)); sys.exit(3)
    seen.append(tid)
print("ok %d" % len(tasks))
PY
}

spec_field() {  # BULLET-PREFIX -> first bullet value
  sed -n "s/^- $1:[[:space:]]*//p" "$SPEC" | sed -n 1p | tr -d '\r'
}

spec_section() {  # HEADER -> section body
  awk -v h="## $1" '{ sub(/\r$/, "", $0) } $0 == h {f=1; next} /^## / {f=0} f' "$SPEC"
}

# Decisions and Interface accumulate across a project's life, and the planner
# call has a prompt budget. Truncating them is worse than refusing: a cut keeps
# the early decisions and silently drops the later ones, which are the ones
# that revised them. So the section goes in whole or the run stops here, before
# any provider call, and says which section to compact.
SPEC_SECTION_BYTES=4096
assert_section_budget() {  # HEADER — refuse in the main shell, before the call
  # Called here and not from inside the prompt substitution on purpose: a fail
  # inside $( ) would exit that subshell and let the run continue with the
  # section silently missing — the failure mode this guard exists to prevent.
  local size
  size="$(spec_section "$1" | wc -c | tr -d ' \r')"
  [ "${size:-0}" -le "$SPEC_SECTION_BYTES" ] ||
    fail "PROJECT.md section '$1' is ${size} bytes, over the ${SPEC_SECTION_BYTES}-byte planner budget; compact it (keep the decisions that still bind) — truncating would drop exactly the later decisions that revised the earlier ones"
}

# Extract one executable acceptance line and the files whose reviewed bytes
# define it. A complete single-backtick Markdown wrapper is presentation, not
# shell syntax; every other backtick shape is rejected so command substitution
# cannot hide inside a contract. Custom commands must name their verification
# surface explicitly. Conventional project check entrypoints may derive it.
acceptance_contract() {  # prints COMMAND, then comma-separated FILES
  python3 - "$SPEC" "$REPO" <<'PY'
import os, re, stat, sys, unicodedata

spec, repo = sys.argv[1:]
try:
    text = open(spec, encoding="utf-8").read()
except (OSError, UnicodeError) as exc:
    sys.stderr.write("error: cannot read PROJECT.md: %s\n" % exc)
    sys.exit(2)
lines = text.splitlines()

def section(name):
    header = "## " + name
    try:
        start = lines.index(header) + 1
    except ValueError:
        return []
    out = []
    for index in range(start, len(lines)):
        if lines[index].startswith("## "):
            break
        out.append((index, lines[index]))
    return out

def field(section_name, label):
    found = []
    pattern = re.compile(r"^- " + re.escape(label) + r":[ \t]*(.*)$")
    body = section(section_name)
    for offset, (line_no, line) in enumerate(body):
        match = pattern.match(line)
        if match:
            found.append((offset, line_no, match.group(1)))
    if len(found) > 1:
        raise ValueError("PROJECT.md has multiple '- %s:' fields" % label)
    if not found:
        return None
    offset, line_no, value = found[0]
    # A Markdown list continuation silently turns one reviewed-looking line
    # into a multi-line shell program in less strict parsers. Refuse any body
    # before the next bullet/header instead of guessing how Markdown folds it.
    for _, following in body[offset + 1:]:
        if following.startswith("- "):
            break
        if following.strip():
            raise ValueError("%s must stay on one Markdown line" % label)
    return value.strip()

def executable(value, label):
    if value is None or not value:
        return ""
    if any(unicodedata.category(ch) in ("Cc", "Cf", "Cs") for ch in value):
        raise ValueError("%s contains a control or format character" % label)
    ticks = value.count("`")
    if ticks:
        if ticks == 2 and value.startswith("`") and value.endswith("`") and len(value) > 2:
            value = value[1:-1].strip()
        else:
            raise ValueError("%s has unmatched/multiple backticks or backtick substitution" % label)
    if not value or value.endswith("\\"):
        raise ValueError("%s is empty or continues onto another line" % label)
    return value

def clean_path(value):
    if (not value or any(unicodedata.category(ch) in ("Cc", "Cf", "Cs") for ch in value) or
            "\\" in value or value.startswith("/") or
            re.match(r"^[A-Za-z]:", value)):
        raise ValueError("Required check files must be normalized repo-relative paths")
    while value.startswith("./"):
        value = value[2:]
    value = value.rstrip("/")
    parts = value.split("/")
    if not value or any(part in ("", ".", "..") for part in parts):
        raise ValueError("Required check files must be normalized repo-relative paths")
    if len(value.encode("utf-8")) > 240:
        raise ValueError("Required check file path exceeds 240 bytes")
    path = os.path.join(repo, *parts)
    try:
        info = os.lstat(path)
    except OSError:
        raise ValueError("Required check file does not exist: %s" % value)
    if not stat.S_ISREG(info.st_mode):
        raise ValueError("Required check file is not a regular non-symlink file: %s" % value)
    physical_repo = os.path.realpath(repo)
    physical_path = os.path.realpath(path)
    if (physical_path != os.path.join(physical_repo, *parts) or
            os.path.commonpath([physical_repo, physical_path]) != physical_repo):
        raise ValueError("Required check file must not traverse a symlink: %s" % value)
    return "/".join(parts)

try:
    raw_command = field("Verification", "Required checks")
    if raw_command is None or not raw_command.strip():
        raw_command = field("Commands", "Test")
    command = executable(raw_command, "Required checks/Test")
    if not command:
        raise ValueError("PROJECT.md has no executable verification command")
    raw_files = field("Verification", "Required check files")
    files = []
    if raw_files:
        values = [part for part in re.split(r"[ \t,]+", raw_files.strip()) if part]
        if len(values) > 64:
            raise ValueError("Required check files accepts at most 64 paths")
        files = sorted(set(clean_path(value) for value in values))
    if not files:
        # Only simple conventional entrances qualify for implicit discovery.
        # Complex/composed commands need an explicit complete surface.
        match = re.fullmatch(
            r"(?:bash|sh)[ \t]+((?:scripts/check|tests/(?:run|check|test|tests))\.sh)"
            r"(?:[ \t]+[-A-Za-z0-9_./:=]+)*", command)
        if match:
            files = [clean_path(match.group(1))]
        else:
            standard = (
                r"make(?:[ \t]+(?:test|check))?",
                r"(?:npm|pnpm|yarn|bun)[ \t]+(?:test|run[ \t]+test)",
                r"(?:python3?|python)[ \t]+-m[ \t]+pytest(?:[ \t]+[-A-Za-z0-9_./:=]+)*",
                r"pytest(?:[ \t]+[-A-Za-z0-9_./:=]+)*",
                r"cargo[ \t]+test(?:[ \t]+[-A-Za-z0-9_./:=]+)*",
                r"go[ \t]+test(?:[ \t]+[-A-Za-z0-9_./:=]+)*",
                r"(?:mvn|gradle|\./gradlew)[ \t]+test(?:[ \t]+[-A-Za-z0-9_./:=]+)*",
            )
            if not any(re.fullmatch(pattern, command) for pattern in standard):
                raise ValueError(
                    "custom acceptance requires '- Required check files:' with every verifier input")
except ValueError as exc:
    sys.stderr.write("error: %s\n" % exc)
    sys.exit(2)

print(command)
print(",".join(files))
PY
}

if [ -n "$APPLY_FILE" ]; then
  [ -f "$APPLY_FILE" ] || fail "no proposal at $APPLY_FILE"
  proposal_meta="$(python3 - "$APPLY_FILE" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    data = json.load(handle)
allowed = data.get("allowed_envelope") or []
print(data.get("spec_sha256") or "-")
print(data.get("plan_sha256") or "-")
print(",".join(str(item) for item in allowed))
print(data.get("id_prefix") or "")
print(",".join(str(item) for item in (data.get("acceptance_files") or [])))
PY
)"
  proposal_meta="${proposal_meta//$'\r'/}"
  proposal_spec_sha="$(printf '%s\n' "$proposal_meta" | sed -n 1p)"
  proposal_plan_sha="$(printf '%s\n' "$proposal_meta" | sed -n 2p)"
  proposal_allowed="$(printf '%s\n' "$proposal_meta" | sed -n 3p)"
  proposal_prefix="$(printf '%s\n' "$proposal_meta" | sed -n 4p)"
  proposal_accept_files="$(printf '%s\n' "$proposal_meta" | sed -n 5p)"
  if [ -n "$ID_PREFIX" ] && [ "$ID_PREFIX" != "$proposal_prefix" ]; then
    fail "--id-prefix does not match the reviewed proposal"
  fi
  ID_PREFIX="$proposal_prefix"
  case "$ID_PREFIX" in
    "") ;;
    *[!A-Za-z0-9._-]*) fail "proposal id_prefix is invalid" ;;
  esac
  out="$(validate_proposal "$APPLY_FILE" 1)" || exit 3
  out="${out//$'\r'/}"
  count="${out#ok }"
  [ -f "$SPEC" ] || fail "PROJECT.md is required to bind a proposal apply"
  goal="$(spec_field Goal)"
  acceptance_meta="$(acceptance_contract)" || exit 2
  acceptance_meta="${acceptance_meta//$'\r'/}"
  accept="$(printf '%s\n' "$acceptance_meta" | sed -n 1p)"
  accept_files="$(printf '%s\n' "$acceptance_meta" | sed -n 2p)"
  [ "$proposal_accept_files" = "$accept_files" ] ||
    fail "proposal acceptance_files do not match the current PROJECT.md verification surface"

  current_spec_sha="$(oms_sha256_file "$SPEC")" || fail "cannot hash PROJECT.md"
  if [ -f "$PLAN_FILE" ]; then
    current_plan_sha="$(oms_sha256_file "$PLAN_FILE")" || fail "cannot hash current plan"
  else
    current_plan_sha=absent
  fi
  [ "$proposal_spec_sha" = - ] || [ "$proposal_spec_sha" = "$current_spec_sha" ] ||
    fail "PROJECT.md changed after the proposal was generated"
  if [ "$proposal_plan_sha" != - ] && [ "$proposal_plan_sha" != "$current_plan_sha" ]; then
    # Exact replay after atomic apply is resolved by agent-plan from immutable
    # task definitions; every other stale plan is rejected there.
    expected_plan_sha="$proposal_plan_sha"
  else
    expected_plan_sha="$current_plan_sha"
  fi
  # agent-plan performs the authoritative clean/sort/set comparison. Keep the
  # wrapper representation-agnostic so `src/` and `src`, or a different list
  # order, remain the same reviewed boundary.
  apply_allowed="${ALLOWED_ENVELOPE:-$proposal_allowed}"
  [ -n "$apply_allowed" ] || apply_allowed=.
  if [ -n "$EXPECTED_PROPOSAL_SHA" ]; then
    # The parent reviewed exact bytes; agent-plan re-reads the file and
    # refuses any drift from this digest inside its atomic apply.
    proposal_sha="$EXPECTED_PROPOSAL_SHA"
  else
    proposal_sha="$(oms_sha256_file "$APPLY_FILE")" || fail "cannot hash proposal"
  fi

  "$ROOT/scripts/agent-plan.sh" --repo "$REPO" apply-proposal \
    --proposal "$APPLY_FILE" \
    --expected-proposal-sha256 "$proposal_sha" \
    --expected-plan-sha256 "$expected_plan_sha" \
    --goal "${goal:-see PROJECT.md}" --accept "$accept" \
    --accept-files "$accept_files" \
    --allowed-envelope "$apply_allowed" --max-tasks "$MAX_TASKS" >/dev/null
  if [ "${OMS_AUTOPILOT:-0}" = 1 ]; then
    echo "plan-from-spec: $count task(s) applied"
  else
    echo "plan-from-spec: $count task(s) applied; parent-agent next: oms goal-drive --repo $REPO"
  fi
  exit 0
fi

# Acceptance-contract preflight for a spec candidate (intent adopt calls
# this so a contract that would refuse at first propose refuses at adopt
# instead — same parser, no drift). State is deliberately not checked: a
# candidate is draft by design; adopting is what confirms it.
if [ -n "$VALIDATE_SPEC" ]; then
  [ -f "$VALIDATE_SPEC" ] || fail "no spec candidate at $VALIDATE_SPEC"
  SPEC="$VALIDATE_SPEC"
  acceptance_meta="$(acceptance_contract)" || exit 2
  acceptance_meta="${acceptance_meta//$'\r'/}"
  echo "validate-spec: acceptance ok ($(printf '%s\n' "$acceptance_meta" | sed -n 1p))"
  exit 0
fi

[ -f "$SPEC" ] || fail "no PROJECT.md in $REPO; scaffold one: apply-project-template.sh auto $REPO"
state="$(sed -n 's/^- State:[[:space:]]*//p' "$SPEC" | sed -n 1p)"
state="${state//$'\r'/}"
case "$state" in
  confirmed|active) ;;
  draft) fail "PROJECT.md is still draft; confirm the spec first (decomposing a draft automates guessing)" ;;
  "") fail "PROJECT.md has no '- State:' field" ;;
  *) fail "PROJECT.md State must be confirmed (legacy active is also accepted)" ;;
esac

acceptance_meta="$(acceptance_contract)" || exit 2
acceptance_meta="${acceptance_meta//$'\r'/}"
accept="$(printf '%s\n' "$acceptance_meta" | sed -n 1p)"
accept_files="$(printf '%s\n' "$acceptance_meta" | sed -n 2p)"

assert_section_budget 'Interface and Data'
assert_section_budget Decisions

spec_sha="$(oms_sha256_file "$SPEC")" || fail "cannot hash PROJECT.md"
if [ -f "$PLAN_FILE" ]; then
  plan_sha="$(oms_sha256_file "$PLAN_FILE")" || fail "cannot hash current plan"
  plan_context="$(python3 - "$PLAN_FILE" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    data = json.load(handle)
for task in data.get("tasks", {}).values():
    print("- %s | %s | %s | allowed=%s" % (
        task.get("id", "?"), task.get("state", "?"), task.get("title", ""),
        ",".join(task.get("allowed_paths") or [])))
PY
)"
  plan_context="${plan_context//$'\r'/}"
else
  plan_sha=absent
  plan_context="- (no plan yet)"
fi
base_sha="$(git -C "$REPO" rev-parse HEAD 2>/dev/null || true)"
[ -n "$base_sha" ] || fail "repository has no HEAD commit"
prefix_rule="ids match [A-Za-z0-9._-]+ and are unique"
[ -z "$ID_PREFIX" ] || prefix_rule="every id starts with $ID_PREFIX and otherwise matches [A-Za-z0-9._-]+"
scope_rule="stay inside PROJECT.md Scope"
[ -z "$ALLOWED_ENVELOPE" ] || scope_rule="every allowed path stays inside this immutable envelope: $ALLOWED_ENVELOPE"

prompt="Decompose the remaining work for this repository into at most $MAX_TASKS
plan tasks. Ground every task in the PROJECT.md contract below; stay inside
Scope, never touch Non-goals.

Return ONLY one JSON object, no prose, matching exactly:
{\"tasks\": [{\"id\": \"t1\", \"title\": \"feat: ...\", \"allowed\": [\"path/prefix\"],
\"verify\": \"one mechanical shell command\", \"depends\": [\"earlier-id\"]}]}

Rules: $prefix_rule; titles are conventional-commit
shaped (they become commit subjects); allowed is a non-empty list of
repo-relative path prefixes the task may touch; verify is one runnable command
that fails while the task is unfinished; depends names only earlier tasks in
the list or an existing DONE task; $scope_rule; never repeat or modify an
existing task; prefer few, landable, independently verifiable tasks.
Verify floor rule: the admission gate re-runs each verify with the task's
own modified files restored from HEAD, so a verify must EXECUTE a suite or
tool (bash tests/x.sh, pytest, a linter) and must still pass under that
restoration. Never write a verify that reads or asserts content that only
exists in the task's patch (grep/sed/cat of an allowed path, inline asserts
of new behavior) — those can never pass the floor; new-behavior proof
belongs in the plan-level acceptance, which runs on the finished tree.
Keep each task within roughly 180 changed lines by default so worker/review
budgets can carry the full patch. If a genuinely indivisible task must exceed
that budget, make the exception explicit in its title so parent review sees it.

--- PROJECT.md contract ---
State: $state
Goal: $(spec_field Goal)
Scope: $(spec_field Scope)
Non-goals: $(spec_field Non-goals)
Commands:
$(spec_section Commands)
Verification:
$(spec_section Verification)
Interface and Data:
$(spec_section 'Interface and Data')
Decisions:
$(spec_section Decisions)
Existing plan (id | state | title | scope):
$plan_context
--- end contract ---"

raw="$(agent_memory_mktemp)" || fail "mktemp failed"
call_args=(--to "$PROVIDER" --repo "$REPO" --operation plan --prompt "$prompt")
[ -z "$MODEL" ] || call_args+=(--model "$MODEL")
[ -z "$FALLBACK_MODEL" ] || call_args+=(--fallback-model "$FALLBACK_MODEL")
[ "$REASONING_EFFORT" = auto ] || call_args+=(--reasoning-effort "$REASONING_EFFORT")
if ! OMS_PEER_TIMEOUT="$PROVIDER_TIMEOUT" "$ROOT/scripts/agent-call.sh" "${call_args[@]}" > "$raw" 2>&1; then
  echo "error: decomposition call failed:" >&2
  tail -n 5 "$raw" >&2
  rm -f "$raw"
  exit 3
fi
# agent-call prints only the artifact path; the answer body lives inside it.
answer_artifact="$(sed -n 's/^artifact: //p' "$raw" | tail -n 1)"
answer_artifact="${answer_artifact//$'\r'/}"
rm -f "$raw"
[ -n "$answer_artifact" ] && [ -f "$answer_artifact" ] ||
  fail "decomposition call produced no artifact"

ts="$(date -u +%Y%m%dT%H%M%SZ)-$$"
mkdir -p "$PLAN_DIR"
agent_memory_ensure_oms_ignore_for_path "$PLAN_DIR" 2>/dev/null || true
proposal="$PLAN_DIR/proposal-$ts.json"
if ! OMS_PLAN_SPEC_SHA="$spec_sha" OMS_PLAN_BEFORE_SHA="$plan_sha" \
  OMS_PLAN_BASE_SHA="$base_sha" OMS_PLAN_ID_PREFIX="$ID_PREFIX" \
  OMS_PLAN_ALLOWED="$ALLOWED_ENVELOPE" OMS_PLAN_ACCEPT_FILES="$accept_files" \
  python3 - "$answer_artifact" > "$proposal" <<'PY'
import json, os, re, sys

text = open(sys.argv[1], encoding="utf-8", errors="replace").read()
# Some provider CLIs echo the whole prompt (schema example included) inside
# their transcript, so scan every balanced JSON object and keep the LAST one
# carrying a "tasks" key — the answer always follows the echo.
marker = text.find("## Output")
if marker >= 0:
    text = text[marker:]
decoder = json.JSONDecoder()
best = None
idx = 0
while True:
    start = text.find("{", idx)
    if start < 0:
        break
    try:
        obj, end = decoder.raw_decode(text, start)
    except ValueError:
        idx = start + 1
        continue
    if isinstance(obj, dict) and "tasks" in obj:
        best = obj
    idx = end if end > start else start + 1
if best is None:
    sys.stderr.write("no tasks JSON object in the provider answer\n")
    sys.exit(3)
allowed = [item for item in re.split(r"[,\s]+", os.environ.get("OMS_PLAN_ALLOWED", "")) if item]
wrapped = {
    "schema": 1,
    "kind": "agent-plan-proposal",
    "spec_sha256": os.environ["OMS_PLAN_SPEC_SHA"],
    "plan_sha256": os.environ["OMS_PLAN_BEFORE_SHA"],
    "base_sha": os.environ["OMS_PLAN_BASE_SHA"],
    "id_prefix": os.environ.get("OMS_PLAN_ID_PREFIX", ""),
    "allowed_envelope": allowed,
    "acceptance_files": [item for item in os.environ.get("OMS_PLAN_ACCEPT_FILES", "").split(",") if item],
    "tasks": best.get("tasks"),
}
json.dump(wrapped, sys.stdout, ensure_ascii=False, indent=2)
PY
then
  rm -f "$proposal"
  echo "error: could not extract a proposal; raw answer kept at $answer_artifact" >&2
  exit 3
fi

out="$(validate_proposal "$proposal")" || {
  echo "error: proposal failed validation; inspect $proposal" >&2
  exit 3
}
out="${out//$'\r'/}"
count="${out#ok }"
proposal_digest="$(oms_sha256_file "$proposal")" || fail "cannot hash the proposal"
echo "plan-from-spec: proposed $count task(s) -> $proposal"
echo "plan-from-spec: proposal sha256: $proposal_digest"
python3 - "$proposal" <<'PY'
import json, re, sys

for t in json.load(open(sys.argv[1], encoding="utf-8"))["tasks"]:
    deps = ",".join(t.get("depends") or []) or "-"
    print("  %-14s %s  [allowed: %s] [verify: %s] [depends: %s]" % (
        t["id"], t["title"], ",".join(t["allowed"]), t["verify"], deps))
    # A task whose verify names a file inside its own scope is admittable only
    # while the patch leaves that file alone: patch-admit refuses a patch that
    # rewrites the test judging it, and asks for verifier-change consent the
    # contract has to carry. When the task's work IS to extend that test -- the
    # ordinary shape for "add a regression" -- the refusal is certain, and it
    # arrives after a worker has spent its whole wall clock. Said here, where
    # the parent is already reading the list and can still change it.
    verify = str(t.get("verify") or "")
    for root in t.get("allowed") or []:
        root = str(root).rstrip("/")
        if not root:
            continue
        if re.search(r"(^|[\s'\"=(])%s(/|[\s'\";)]|$)" % re.escape(root), verify):
            print("    note: verify reads %s, which this task may also change;"
                  " a patch that touches it is refused without verifier-change"
                  " consent" % root)
            break
PY
# Under autopilot the sole continuation is the digest-bound autopilot run,
# which the parent prints itself; standalone advice would bypass that entrance.
if [ "${OMS_AUTOPILOT:-0}" != 1 ]; then
  echo "review the list, then: oms plan-from-spec --repo $REPO --apply $proposal --expected-proposal-sha256 $proposal_digest"
fi
exit 0
