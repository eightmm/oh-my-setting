#!/usr/bin/env bash
set -euo pipefail

# Vague goal -> reviewed intent spec -> PROJECT.md. The delegation gap
# (Anthropic 2026 trends: ~60% AI usage, 0-20% full delegation) is blocked by
# missing persistent context, testable outcomes, and explicit constraints —
# exactly the fields PROJECT.md already carries for plan-from-spec/autopilot.
# This front door closes the entrance: `draft` asks a provider to turn one
# goal sentence into a structured candidate spec (a durable file, never a
# vanished prompt), the operator reviews and may edit it, and `adopt` — the
# explicit approval act — is the only step that writes PROJECT.md. Generated
# specs never confirm themselves. The chain it opens:
#   goal sentence -> intent draft -> [review] -> intent adopt
#     -> plan-from-spec -> [review] -> goal-drive / autopilot.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$SCRIPT_DIR/lib/agent-memory-common.sh"

ACTION=""
REPO="$PWD"
PROVIDER="codex"
MODEL=""
FALLBACK_MODEL=""
REASONING_EFFORT="auto"
PROVIDER_TIMEOUT="${OMS_PEER_TIMEOUT:-5m}"
GOAL_TEXT=""
INTENT_ID=""

usage() {
  cat <<'EOF'
usage:
  intent.sh draft --goal "TEXT" [--to PROVIDER] [--repo PATH]
                  [--model M] [--fallback-model M] [--reasoning-effort E]
  intent.sh show [--id ID] [--repo PATH]
  intent.sh adopt --id ID [--repo PATH]

Turn one goal sentence into a durable, reviewed PROJECT.md contract.

draft   Ask a provider to expand the goal into a structured intent spec
        (Goal/Scope/Non-goals, Commands, a single-line Required checks
        acceptance, edge cases). Writes a candidate under .oms/intents/
        with provenance; never touches PROJECT.md. Review the candidate —
        editing it is expected — then adopt.
show    List candidates, or print one with --id.
adopt   The explicit approval act and the only PROJECT.md writer. Refuses
        while a live autopilot receipt exists (a spec swap under a live
        run only bricks it — abandon first), refuses when PROJECT.md
        already exists (back it up or remove it deliberately), refuses an
        acceptance that already passes (a pre-passing check cannot prove
        the work), and warns when the acceptance reads scope-file content
        (planners must not copy that shape into task verifies — the
        admission floor rejects it). Writes PROJECT.md with
        `- State: confirmed`: adopting IS the confirmation.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --to) [ "$#" -ge 2 ] || fail "--to requires a provider"; PROVIDER="$2"; shift 2 ;;
    --goal) [ "$#" -ge 2 ] || fail "--goal requires text"; GOAL_TEXT="$2"; shift 2 ;;
    --id) [ "$#" -ge 2 ] || fail "--id requires an intent id"; INTENT_ID="$2"; shift 2 ;;
    --model) [ "$#" -ge 2 ] || fail "--model requires a name"; MODEL="$2"; shift 2 ;;
    --fallback-model)
      [ "$#" -ge 2 ] || fail "--fallback-model requires a name"
      FALLBACK_MODEL="$2"; shift 2 ;;
    --reasoning-effort)
      [ "$#" -ge 2 ] || fail "--reasoning-effort requires a level"
      REASONING_EFFORT="$2"; shift 2 ;;
    --provider-timeout)
      [ "$#" -ge 2 ] || fail "--provider-timeout requires a duration"
      PROVIDER_TIMEOUT="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    draft|show|adopt)
      [ -z "$ACTION" ] || fail "multiple actions: $ACTION, $1"
      ACTION="$1"; shift ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ -n "$ACTION" ] || { usage >&2; exit 2; }
REPO="$(cd "$REPO" && pwd)" || fail "bad --repo"
INTENT_DIR="$REPO/.oms/intents"
SPEC="$REPO/PROJECT.md"
OUTER_RECEIPT="$REPO/.oms/plan/autopilot-run.json"

# Bare names only: an id with a slash is a path, and paths are not ids. The
# same rule the handoff store follows.
require_intent_file() {
  case "$INTENT_ID" in
    "") fail "--id is required; see: intent.sh show" ;;
    */*|*\\*|.*) fail "intent ids are bare names inside .oms/intents (see: intent.sh show)" ;;
  esac
  INTENT_FILE="$INTENT_DIR/$INTENT_ID.md"
  [ -f "$INTENT_FILE" ] || fail "no intent candidate named $INTENT_ID (see: intent.sh show)"
}

# Structural validation shared by draft (on write) and adopt (the file is
# expected to have been hand-edited between the two — review edits are the
# point — so adopt re-validates rather than trusting its own earlier pass).
validate_intent_file() {  # FILE
  python3 - "$1" <<'PY'
import re, sys, unicodedata

path = sys.argv[1]
text = open(path, encoding="utf-8", errors="strict").read()
lines = text.splitlines()

def die(message):
    sys.stderr.write("error: intent candidate: %s\n" % message)
    raise SystemExit(3)

def section(name):
    header = "## " + name
    try:
        start = lines.index(header) + 1
    except ValueError:
        die("missing required section '## %s'" % name)
    body = []
    for line in lines[start:]:
        if line.startswith("## "):
            break
        body.append(line)
    return body

def bullet(body, label, section_name):
    pattern = re.compile(r"^- " + re.escape(label) + r":[ \t]*(.*)$")
    found = []
    for index, line in enumerate(body):
        match = pattern.match(line)
        if match:
            found.append((index, match.group(1).strip()))
    if len(found) > 1:
        die("multiple '- %s:' bullets in '## %s'" % (label, section_name))
    if not found:
        die("missing '- %s:' bullet in '## %s'" % (label, section_name))
    index, value = found[0]
    for following in body[index + 1:]:
        if following.startswith("- ") or not following.strip():
            break
        die("'- %s:' must stay on one Markdown line" % label)
    if not value:
        die("'- %s:' is empty" % label)
    if any(unicodedata.category(ch) in ("Cc", "Cf", "Cs") for ch in value):
        die("'- %s:' contains a control or format character" % label)
    return value

status = section("Status")
state = bullet(status, "State", "Status")
project = section("Project")
bullet(project, "Goal", "Project")
bullet(project, "Scope", "Project")
bullet(project, "Non-goals", "Project")
section("Commands")
verification = section("Verification")
checks = bullet(verification, "Required checks", "Verification")
# Same executable-line rules plan-from-spec enforces: a single-backtick wrap
# is presentation; every other backtick shape could hide substitution.
ticks = checks.count("`")
if ticks:
    if ticks == 2 and checks.startswith("`") and checks.endswith("`") and len(checks) > 2:
        checks = checks[1:-1].strip()
    else:
        die("'- Required checks:' has unmatched/multiple backticks")
if not checks or checks.endswith("\\"):
    die("'- Required checks:' is empty or continues onto another line")
print(state)
print(checks)
PY
}

# Scope bullets that look like repo paths, for the acceptance floor warning.
scope_paths() {  # FILE
  python3 - "$1" <<'PY'
import re, sys

lines = open(sys.argv[1], encoding="utf-8", errors="replace").read().splitlines()
scope = ""
in_project = False
for line in lines:
    if line.startswith("## "):
        in_project = line == "## Project"
        continue
    if in_project:
        match = re.match(r"^- Scope:[ \t]*(.*)$", line)
        if match:
            scope = match.group(1)
            break
# Every path-charset entry rides along; the linter matches verify words
# against them EXACTLY, so prose entries simply never hit.
for entry in re.split(r"[ \t,]+", scope):
    entry = entry.strip().rstrip("/")
    if entry and re.match(r"^[A-Za-z0-9._/-]+$", entry):
        print(entry)
PY
}

if [ "$ACTION" = show ]; then
  if [ -n "$INTENT_ID" ]; then
    require_intent_file
    cat "$INTENT_FILE"
    exit 0
  fi
  [ -d "$INTENT_DIR" ] || { echo "intent: no candidates"; exit 0; }
  found=0
  for candidate in "$INTENT_DIR"/*.md; do
    [ -f "$candidate" ] || continue
    found=1
    name="$(basename "$candidate" .md)"
    goal_line="$(sed -n 's/^- Goal:[[:space:]]*//p' "$candidate" | sed -n 1p)"
    printf '%s\t%s\n' "$name" "${goal_line:-"(no goal bullet)"}"
  done
  [ "$found" -eq 1 ] || echo "intent: no candidates"
  exit 0
fi

if [ "$ACTION" = draft ]; then
  [ -n "$GOAL_TEXT" ] || fail "draft requires --goal \"TEXT\""
  [ "${#GOAL_TEXT}" -le 2000 ] || fail "--goal is longer than 2000 characters; compact it"
  case "$GOAL_TEXT" in
    *$'\t'*) fail "--goal must not contain tab characters" ;;
  esac

  existing_spec=""
  if [ -f "$SPEC" ]; then
    existing_spec="$(head -c 8192 "$SPEC")"
  fi
  tree_listing="$(cd "$REPO" && ls -1 | head -40)"

  prompt="Turn ONE goal sentence into a durable intent specification for this
repository. The spec is a delegation contract: an agent must be able to
execute it end-to-end without follow-up questions. Anything you would have
to ask about goes under '- Open decisions:' instead of being guessed.

Return ONLY a Markdown document (no code fences, no prose before or after)
matching exactly this skeleton:

## Status

- State: draft
- Open decisions: <anything a human must settle, or 'none'>

## Project

- Goal: <one line: what done means>
- Scope: <one line: repo-relative path prefixes the work may touch>
- Non-goals: <one line: what this explicitly does not do>

## Commands

- Test: <how this repository runs its tests>

## Verification

- Success criteria: <one line: observable outcome>
- Required checks: <ONE runnable shell command, on one line, that FAILS
  while the work is unfinished and passes when it is done>

## Edge cases

- <the edge cases the implementation must survive, one bullet each>

## Notes

- Risks: <what could go wrong>
- Do not touch: <boundaries>

Rules: every bullet shown above must be present and non-empty; Goal, Scope,
Non-goals and Required checks each stay on one line; Required checks is a
real command against this repository, not a placeholder; Scope names real
path prefixes from the repository listing below.

--- goal sentence ---
$GOAL_TEXT
--- repository top-level listing ---
$tree_listing
--- existing PROJECT.md (refine, do not contradict; empty if none) ---
$existing_spec
--- end context ---"

  raw="$(agent_memory_mktemp)" || fail "mktemp failed"
  call_args=(--to "$PROVIDER" --repo "$REPO" --operation plan --prompt "$prompt")
  [ -z "$MODEL" ] || call_args+=(--model "$MODEL")
  [ -z "$FALLBACK_MODEL" ] || call_args+=(--fallback-model "$FALLBACK_MODEL")
  [ "$REASONING_EFFORT" = auto ] || call_args+=(--reasoning-effort "$REASONING_EFFORT")
  if ! OMS_PEER_TIMEOUT="$PROVIDER_TIMEOUT" "$ROOT/scripts/agent-call.sh" "${call_args[@]}" > "$raw" 2>&1; then
    echo "error: intent draft call failed:" >&2
    tail -n 5 "$raw" >&2
    rm -f "$raw"
    exit 3
  fi
  answer_artifact="$(sed -n 's/^artifact: //p' "$raw" | tail -n 1)"
  answer_artifact="${answer_artifact//$'\r'/}"
  rm -f "$raw"
  [ -n "$answer_artifact" ] && [ -f "$answer_artifact" ] ||
    fail "intent draft call produced no artifact"

  body="$(agent_memory_mktemp)" || fail "mktemp failed"
  # The transcript can echo the skeleton from the prompt; the answer always
  # follows the echo, so the LAST '## Status' anchor wins. Harness footer
  # lines (stop-reason, tokens, model-result) and fence lines are transport,
  # not spec.
  if ! python3 - "$answer_artifact" > "$body" <<'PY'
import re, sys

text = open(sys.argv[1], encoding="utf-8", errors="replace").read()
# Only the transport's Output window speaks: the artifact's own Prompt
# section quotes the skeleton from the instructions, and a quoted skeleton
# is not the answer (the same window rule the verdict readers follow).
windows = text.split("\n## Output\n")
if len(windows) > 1:
    text = windows[-1]
lines = [line for line in text.splitlines() if line.strip() not in ("```", "```markdown", "```md")]
anchors = [index for index, line in enumerate(lines) if line.strip() == "## Status"]
if not anchors:
    sys.stderr.write("error: the answer carries no '## Status' skeleton\n")
    raise SystemExit(3)
kept = lines[anchors[-1]:]
footer = re.compile(r"^(stop-reason: |tokens used$|[0-9][0-9,]*$|model-result: |model-route: |model-fallback: )")
while kept and (not kept[-1].strip() or footer.match(kept[-1])):
    kept.pop()
print("\n".join(kept))
PY
  then
    rm -f "$body"
    echo "error: could not extract an intent spec (artifact kept: $answer_artifact)" >&2
    exit 3
  fi

  if ! validate_intent_file "$body" >/dev/null; then
    rm -f "$body"
    echo "error: the drafted spec failed validation (artifact kept: $answer_artifact)" >&2
    exit 3
  fi

  mkdir -p "$INTENT_DIR"
  agent_memory_ensure_oms_ignore "$REPO"
  ts="$(date -u +%Y%m%dT%H%M%SZ)"
  intent_name="intent-$ts"
  target="$INTENT_DIR/$intent_name.md"
  {
    printf '## Provenance\n\n'
    printf -- '- Drafted by: %s (intent.sh; content below is model output — review before adopt)\n' "$PROVIDER"
    printf -- '- Drafted at: %s\n' "$ts"
    printf -- '- Goal sentence: %s\n' "$GOAL_TEXT"
    printf -- '- Artifact: %s\n\n' "$answer_artifact"
    cat "$body"
    printf '\n'
  } > "$target"
  rm -f "$body"
  echo "intent: drafted $intent_name"
  echo "intent: review it (editing is expected): $target"
  echo "intent: then: intent.sh adopt --id $intent_name --repo $REPO"
  exit 0
fi

if [ "$ACTION" = adopt ]; then
  require_intent_file

  # A spec swap under a live autopilot run only bricks it into spec-changed
  # park — the same immutability the receipt enforces, said one step earlier.
  if [ -e "$OUTER_RECEIPT" ] || [ -L "$OUTER_RECEIPT" ]; then
    fail "a live autopilot receipt exists; adopting a new contract under it would only brick the run — finish it or run: oms autopilot abandon --reason ..."
  fi
  if [ -e "$SPEC" ] || [ -L "$SPEC" ]; then
    fail "PROJECT.md already exists; back it up or remove it deliberately before adopting a new contract"
  fi

  meta="$(validate_intent_file "$INTENT_FILE")" || exit 3
  accept_cmd="$(printf '%s\n' "$meta" | sed -n 2p)"

  # The vacuity gate (a field defect class: hollow acceptance): an acceptance
  # that passes before any work exists cannot prove the work. Required checks
  # must fail while the work is unfinished — run it once, expect failure.
  accept_rc=0
  OMS_INTENT_ACCEPT_CMD="$accept_cmd" OMS_INTENT_REPO="$REPO" \
    python3 - <<'PY' || accept_rc=$?
import os, subprocess, sys

command = os.environ["OMS_INTENT_ACCEPT_CMD"]
repo = os.environ["OMS_INTENT_REPO"]
try:
    result = subprocess.run(
        ["bash", "-c", command],
        cwd=repo,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        timeout=int(os.environ.get("OMS_INTENT_ACCEPT_TIMEOUT", "120")),
    )
except subprocess.TimeoutExpired:
    sys.exit(124)
sys.exit(result.returncode)
PY
  if [ "$accept_rc" -eq 0 ]; then
    fail "Required checks already passes with no work done — it cannot prove the work; sharpen it until it fails while the goal is unfinished"
  fi
  if [ "$accept_rc" -eq 124 ]; then
    fail "Required checks did not finish within \${OMS_INTENT_ACCEPT_TIMEOUT:-120}s; an acceptance the operator cannot afford to run is not a contract"
  fi

  # Acceptance-level content reads of scope files are legitimate (they fail
  # while unfinished, pass when done) — but the same shape copied into a task
  # verify dies at the admission floor. Warn so planners are not seeded with
  # a floor-incompatible pattern. Warning, not refusal: refusing would block
  # exactly the fails-until-done acceptance this gate demands.
  scope_list="$(scope_paths "$INTENT_FILE" | tr '\n' ',' | sed 's/,$//')"
  if [ -n "$scope_list" ]; then
    lint_out="$("$ROOT/scripts/agent-plan.sh" --repo "$REPO" lint-verify \
      --verify "$accept_cmd" --allowed "$scope_list" 2>&1)" || true
    case "$lint_out" in
      *floor_incompatible_verifier*)
        echo "warning: the acceptance reads scope-file content ($lint_out)" >&2
        echo "warning: fine as plan acceptance; planners must NOT copy this shape into task verifies — the admission floor rejects those" >&2
        ;;
    esac
  fi

  # Adopting IS the confirmation: the operator (or parent agent) invoking
  # adopt is the explicit approval act plan-from-spec's draft-refusal points
  # at. Everything else in the candidate is preserved byte-for-byte,
  # provenance included.
  tmp_spec="$(agent_memory_mktemp)" || fail "mktemp failed"
  sed 's/^- State: draft$/- State: confirmed/' "$INTENT_FILE" > "$tmp_spec"
  grep -q '^- State: confirmed$' "$tmp_spec" ||
    { rm -f "$tmp_spec"; fail "the candidate's '- State:' is not draft; only draft candidates adopt"; }
  mv "$tmp_spec" "$SPEC"
  echo "intent: adopted $INTENT_ID -> PROJECT.md (State: confirmed)"
  echo "intent: next: oms plan-from-spec --repo $REPO   (or: oms autopilot propose ...)"
  exit 0
fi

fail "unknown action: $ACTION"
