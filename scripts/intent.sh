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

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd -P)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$SCRIPT_DIR/lib/agent-memory-common.sh"
# shellcheck source=scripts/lib/work-journal.sh
. "$SCRIPT_DIR/lib/work-journal.sh"

ACTION=""
REPO="$PWD"
PROVIDER="codex"
MODEL=""
FALLBACK_MODEL=""
FALLBACK_PROVIDER=""
FALLBACK_PROVIDER_SET=0
REASONING_EFFORT="auto"
PROVIDER_TIMEOUT="${OMS_PEER_TIMEOUT:-5m}"
GOAL_TEXT=""
INTENT_ID=""

usage() {
  cat <<'EOF'
usage:
  intent.sh draft --goal "TEXT" [--to PROVIDER] [--repo PATH]
                  [--model M] [--fallback-model M] [--fallback-to PROVIDER|none]
                  [--reasoning-effort E] [--provider-timeout DUR]
  intent.sh show [--id ID] [--repo PATH]
  intent.sh adopt --id ID [--repo PATH]

Turn one goal sentence into a durable, reviewed PROJECT.md contract.

draft   Ask a provider to expand the goal into a structured intent spec
        (Goal/Scope/Non-goals, Commands, a single-line Required checks
        acceptance, edge cases). Writes a candidate under .oms/intents/
        with provenance; never touches PROJECT.md. Review the candidate —
        editing it is expected — then adopt. A transient failed call retries
        once on the other installed core provider by default; --fallback-to
        none disables that retry.
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
    --fallback-to)
      [ "$#" -ge 2 ] || fail "--fallback-to requires a provider or 'none'"
      FALLBACK_PROVIDER="$2"; FALLBACK_PROVIDER_SET=1; shift 2 ;;
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
REPO="$(cd "$REPO" && pwd -P)" || fail "bad --repo"
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

# Freeze one candidate generation before any gate runs. Opening the leaf with
# O_NOFOLLOW and checking the descriptor/path identity on both sides catches
# ordinary editor replacement and in-place writes; the bounded second read
# catches a same-size write that overlaps the first. This is a cooperative
# filesystem fence, not a claim against a hostile same-UID process.
snapshot_intent_file() {  # SOURCE DESTINATION
  python3 - "$1" "$2" <<'PY'
import hashlib
import os
import stat
import sys

source, destination = sys.argv[1:]
limit = 1024 * 1024


def die(message):
    sys.stderr.write("error: intent candidate snapshot: %s\n" % message)
    raise SystemExit(3)


def is_link(path):
    if os.path.islink(path):
        return True
    isjunction = getattr(os.path, "isjunction", None)
    return bool(isjunction and isjunction(path))


if is_link(source):
    die("candidate must be a regular file, not a link or junction")
flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
try:
    descriptor = os.open(source, flags)
except OSError as exc:
    die("cannot open candidate without following links: %s" % exc)
try:
    before = os.fstat(descriptor)
    if not stat.S_ISREG(before.st_mode):
        die("candidate is not a regular file")
    if before.st_size > limit:
        die("candidate exceeds the 1 MiB limit")

    def read_all():
        chunks = []
        total = 0
        while True:
            chunk = os.read(descriptor, min(64 * 1024, limit + 1 - total))
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
            if total > limit:
                die("candidate exceeds the 1 MiB limit")
        return b"".join(chunks)

    first = read_all()
    middle = os.fstat(descriptor)
    os.lseek(descriptor, 0, os.SEEK_SET)
    second = read_all()
    after = os.fstat(descriptor)
finally:
    os.close(descriptor)

fields = ("st_dev", "st_ino", "st_mode", "st_size", "st_mtime_ns", "st_ctime_ns")
if any(getattr(before, name, None) != getattr(middle, name, None) for name in fields):
    die("candidate changed while it was being read")
if any(getattr(middle, name, None) != getattr(after, name, None) for name in fields):
    die("candidate changed while it was being read")
if first != second:
    die("candidate changed while it was being read")
try:
    live = os.lstat(source)
except OSError as exc:
    die("candidate pathname changed while it was being read: %s" % exc)
if is_link(source) or not stat.S_ISREG(live.st_mode):
    die("candidate pathname changed while it was being read")
identity = ("st_dev", "st_ino")
if any(getattr(live, name, None) != getattr(after, name, None) for name in identity):
    die("candidate pathname changed while it was being read")

out_flags = os.O_WRONLY | os.O_TRUNC | getattr(os, "O_BINARY", 0)
out_flags |= getattr(os, "O_NOFOLLOW", 0)
try:
    output = os.open(destination, out_flags)
except OSError as exc:
    die("cannot open snapshot destination: %s" % exc)
try:
    if not stat.S_ISREG(os.fstat(output).st_mode):
        die("snapshot destination is not a regular file")
    offset = 0
    while offset < len(first):
        offset += os.write(output, first[offset:])
    os.fsync(output)
finally:
    os.close(output)
print(hashlib.sha256(first).hexdigest())
PY
}

confirmed_intent_snapshot() {  # SNAPSHOT STAGED_PROJECT
  python3 - "$1" "$2" <<'PY'
import os
import stat
import sys

source, destination = sys.argv[1:]
try:
    with open(source, "rb") as handle:
        encoded = handle.read(1024 * 1024 + 1)
except OSError as exc:
    sys.stderr.write("error: cannot read frozen intent candidate: %s\n" % exc)
    raise SystemExit(3)
if len(encoded) > 1024 * 1024:
    raise SystemExit(3)
try:
    text = encoded.decode("utf-8", errors="strict")
except UnicodeDecodeError as exc:
    sys.stderr.write("error: frozen intent candidate is not UTF-8: %s\n" % exc)
    raise SystemExit(3)
lines = text.splitlines(True)
in_status = False
matches = []
for index, line in enumerate(lines):
    plain = line.rstrip("\r\n")
    if plain.startswith("## "):
        in_status = plain == "## Status"
        continue
    if in_status and plain == "- State: draft":
        matches.append(index)
if len(matches) != 1:
    sys.stderr.write("error: the candidate's '- State:' is not draft; only draft candidates adopt\n")
    raise SystemExit(2)
index = matches[0]
ending = lines[index][len(lines[index].rstrip("\r\n")):]
lines[index] = "- State: confirmed" + ending
result = "".join(lines).encode("utf-8")
flags = os.O_WRONLY | os.O_TRUNC | getattr(os, "O_BINARY", 0)
flags |= getattr(os, "O_NOFOLLOW", 0)
try:
    descriptor = os.open(destination, flags)
    try:
        if not stat.S_ISREG(os.fstat(descriptor).st_mode):
            raise OSError("staged PROJECT.md is not a regular file")
        offset = 0
        while offset < len(result):
            offset += os.write(descriptor, result[offset:])
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
except OSError as exc:
    sys.stderr.write("error: cannot stage confirmed PROJECT.md: %s\n" % exc)
    raise SystemExit(3)
PY
}

atomic_create_project() {  # STAGED_PROJECT PROJECT
  python3 - "$1" "$2" <<'PY'
import os
import sys

staged, target = sys.argv[1:]
try:
    os.link(staged, target)
except FileExistsError:
    raise SystemExit(17)
except OSError as exc:
    sys.stderr.write("error: cannot atomically publish PROJECT.md: %s\n" % exc)
    raise SystemExit(3)

# The hard link is the no-clobber publication point. Sync the directory where
# the platform permits it; native Windows does not expose directory fsync.
try:
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
    directory = os.open(os.path.dirname(target) or ".", flags)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)
except OSError:
    # Publication already happened atomically. Some filesystems and native
    # Windows reject directory fsync; treating that as a failed publication
    # would lie to the caller while PROJECT.md is already visible.
    pass
PY
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
# against them EXACTLY, so prose entries simply never hit. Backtick wrap is
# presentation, not path (observed on the verb's first field draft: every
# scope entry arrived backticked and the floor warning silently skipped).
for entry in re.split(r"[ \t,]+", scope):
    entry = entry.strip().strip("`").rstrip("/")
    if entry and re.match(r"^[A-Za-z0-9._/-]+$", entry):
        print(entry)
PY
}

# The transport's Prompt section is input, not an answer. Keep this one window
# rule shared by retry classification and candidate extraction so a goal word
# in the outbound prompt cannot spend the fallback seat.
intent_artifact_output_window() {  # ARTIFACT
  python3 - "$1" <<'PY'
import re
import sys

try:
    text = open(sys.argv[1], encoding="utf-8", errors="replace").read()
except OSError as exc:
    sys.stderr.write("error: could not read intent call artifact: %s\n" % exc)
    raise SystemExit(3)
windows = text.split("\n## Output\n")
if len(windows) > 1:
    text = windows[-1]
sys.stdout.write(re.split(r"\n## Exit\b", text)[0])
PY
}

# A retriable failure is deliberately narrow. Exit 3 means the outbound
# scrubber refused the same prompt and 127 means the seat is absent, so neither
# can improve by sending an identical prompt to another provider.
intent_first_call_is_transient() {  # EXIT_CODE OUTPUT_WINDOW
  python3 - "$1" "$2" <<'PY'
import re
import sys

status = int(sys.argv[1])
try:
    text = open(sys.argv[2], encoding="utf-8", errors="replace").read()
except OSError:
    raise SystemExit(1)
if status in (3, 127):
    raise SystemExit(1)
if status == 124:
    raise SystemExit(0)
if re.search(r"529|overloaded|rate limit", text, re.IGNORECASE):
    raise SystemExit(0)
# The router records route/footer metadata even when the provider answered
# nothing; that transport text must not turn an empty answer into a refusal.
lines = text.splitlines()
transport = re.compile(r"^(seat-health: |model-route: |model-result: |model-fallback: |stop-reason: )")
paired = {"served model", "configured model", "cost usd", "tokens used"}
while lines:
    tail = lines[-1].strip()
    if not tail or transport.match(tail):
        lines.pop()
    elif len(lines) >= 2 and lines[-2].strip().lower() in paired:
        lines.pop()
        lines.pop()
    else:
        break
if not "\n".join(lines).strip():
    raise SystemExit(0)
raise SystemExit(1)
PY
}

intent_doubled_timeout() {  # DURATION
  python3 - "$1" <<'PY'
import re
import sys

value = sys.argv[1]
match = re.fullmatch(r"([0-9]+)([smh]?)", value)
if not match:
    print(value)
else:
    print("%d%s" % (int(match.group(1)) * 2, match.group(2)))
PY
}

# Provenance survives into a later outbound PROJECT.md prompt. Artifact paths
# under this repository therefore stay repo-relative instead of becoming an
# absolute-machine-path scrubber failure on the next draft.
intent_artifact_ref() {  # ARTIFACT
  local artifact_ref="$1"
  case "$artifact_ref" in
    "$REPO"/*) artifact_ref="${artifact_ref#"$REPO"/}" ;;
  esac
  printf '%s\n' "$artifact_ref"
}

intent_draft_artifact_pointers() {  # FIRST [FALLBACK]
  local first="$1"
  local fallback="${2:-}"
  [ -z "$first" ] || printf 'artifact kept: %s\n' "$first" >&2
  [ -z "$fallback" ] || printf 'fallback artifact kept: %s\n' "$fallback" >&2
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

  fallback_provider=""
  if [ "$FALLBACK_PROVIDER_SET" -eq 1 ]; then
    case "$FALLBACK_PROVIDER" in
      none) ;;
      *)
        fallback_provider="$(oms_provider_normalize "$FALLBACK_PROVIDER" 2>/dev/null)" ||
          fail "--fallback-to requires a supported provider or 'none'"
        fallback_provider="${fallback_provider//$'\r'/}"
        primary_provider="${PROVIDER%%:*}"
        primary_provider="$(oms_provider_normalize "$primary_provider" 2>/dev/null)" ||
          primary_provider="${PROVIDER%%:*}"
        primary_provider="${primary_provider//$'\r'/}"
        [ "$fallback_provider" != "$primary_provider" ] ||
          fail "--fallback-to must name a provider different from --to"
        ;;
    esac
  else
    case "$PROVIDER" in
      codex) fallback_provider="claude" ;;
      claude) fallback_provider="codex" ;;
    esac
    # Default only to a seat that physically resolves on PATH. An explicit
    # fallback is still attempted so its own artifact records a missing binary.
    if [ -n "$fallback_provider" ] &&
      ! oms_provider_cli_discovered "$fallback_provider"; then
      fallback_provider=""
    fi
  fi

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
- Required check files: <the existing repo files that command reads —
  REQUIRED for any custom command; omit the bullet entirely only when the
  command is a conventional entrypoint such as bash tests/run.sh or pytest>

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

  intent_draft_raw=""
  intent_fallback_raw=""
  intent_answer_window=""
  body=""
  intent_draft_cleanup() {
    local file
    for file in "$intent_draft_raw" "$intent_fallback_raw" \
      "$intent_answer_window" "$body"; do
      [ -z "$file" ] || rm -f "$file"
    done
  }
  trap intent_draft_cleanup EXIT

  intent_draft_raw="$(agent_memory_mktemp)" || fail "mktemp failed"
  call_args=(--to "$PROVIDER" --repo "$REPO" --operation plan --prompt "$prompt")
  [ -z "$MODEL" ] || call_args+=(--model "$MODEL")
  [ -z "$FALLBACK_MODEL" ] || call_args+=(--fallback-model "$FALLBACK_MODEL")
  [ "$REASONING_EFFORT" = auto ] || call_args+=(--reasoning-effort "$REASONING_EFFORT")
  first_rc=0
  OMS_PEER_TIMEOUT="$PROVIDER_TIMEOUT" "$ROOT/scripts/agent-call.sh" "${call_args[@]}" \
    > "$intent_draft_raw" 2>&1 || first_rc=$?
  first_artifact="$(sed -n 's/^artifact: //p' "$intent_draft_raw" | tail -n 1)"
  first_artifact="${first_artifact//$'\r'/}"
  fallback_artifact=""
  if [ -z "$first_artifact" ] || [ ! -f "$first_artifact" ]; then
    if [ "$first_rc" -ne 0 ]; then
      echo "error: intent draft call failed:" >&2
      tail -n 5 "$intent_draft_raw" >&2
    else
      echo "error: intent draft call produced no artifact" >&2
    fi
    intent_draft_artifact_pointers "$first_artifact"
    exit 3
  fi

  intent_answer_window="$(agent_memory_mktemp)" || fail "mktemp failed"
  if ! intent_artifact_output_window "$first_artifact" > "$intent_answer_window"; then
    echo "error: could not read the intent draft answer (artifact kept: $first_artifact)" >&2
    intent_draft_artifact_pointers "$first_artifact"
    exit 3
  fi

  answer_artifact="$first_artifact"
  answering_provider="$PROVIDER"
  if [ -n "$fallback_provider" ] &&
    intent_first_call_is_transient "$first_rc" "$intent_answer_window"; then
    if ! fallback_timeout="$(intent_doubled_timeout "$PROVIDER_TIMEOUT")"; then
      fallback_timeout="$PROVIDER_TIMEOUT"
    fi
    fallback_timeout="${fallback_timeout//$'\r'/}"
    intent_fallback_raw="$(agent_memory_mktemp)" || fail "mktemp failed"
    # A model route is provider-family-specific. The fallback gets its normal
    # route while the generic reasoning effort remains intact.
    fallback_args=(--to "$fallback_provider" --repo "$REPO" --operation plan --prompt "$prompt")
    [ "$REASONING_EFFORT" = auto ] || fallback_args+=(--reasoning-effort "$REASONING_EFFORT")
    fallback_rc=0
    OMS_PEER_TIMEOUT="$fallback_timeout" "$ROOT/scripts/agent-call.sh" "${fallback_args[@]}" \
      > "$intent_fallback_raw" 2>&1 || fallback_rc=$?
    fallback_artifact="$(sed -n 's/^artifact: //p' "$intent_fallback_raw" | tail -n 1)"
    fallback_artifact="${fallback_artifact//$'\r'/}"
    if [ -z "$fallback_artifact" ] || [ ! -f "$fallback_artifact" ]; then
      echo "error: intent draft fallback call produced no artifact:" >&2
      tail -n 5 "$intent_fallback_raw" >&2
      intent_draft_artifact_pointers "$first_artifact" "$fallback_artifact"
      exit 3
    fi
    if [ "$fallback_rc" -ne 0 ]; then
      echo "error: intent draft fallback call failed:" >&2
      tail -n 5 "$intent_fallback_raw" >&2
      intent_draft_artifact_pointers "$first_artifact" "$fallback_artifact"
      exit 3
    fi
    answer_artifact="$fallback_artifact"
    answering_provider="$fallback_provider"
    if ! intent_artifact_output_window "$answer_artifact" > "$intent_answer_window"; then
      echo "error: could not read the fallback intent draft answer (artifact kept: $answer_artifact)" >&2
      intent_draft_artifact_pointers "$first_artifact" "$fallback_artifact"
      exit 3
    fi
  elif [ "$first_rc" -ne 0 ]; then
    echo "error: intent draft call failed:" >&2
    tail -n 5 "$intent_draft_raw" >&2
    intent_draft_artifact_pointers "$first_artifact"
    exit 3
  fi

  body="$(agent_memory_mktemp)" || fail "mktemp failed"
  # The output window is already isolated above. The transcript can echo the
  # skeleton from the prompt, so the LAST '## Status' anchor wins; transport
  # footer and fence lines are not part of the candidate.
  if ! python3 - "$intent_answer_window" > "$body" <<'PY'
import re, sys

text = open(sys.argv[1], encoding="utf-8", errors="replace").read()
lines = [line for line in text.splitlines() if line.strip() not in ("```", "```markdown", "```md")]
anchors = [index for index, line in enumerate(lines) if line.strip() == "## Status"]
if not anchors:
    sys.stderr.write("error: the answer carries no '## Status' skeleton\n")
    raise SystemExit(3)
kept = lines[anchors[-1]:]
footer = re.compile(r"^(stop-reason: |tokens used$|[0-9][0-9,]*$|model-result: |model-route: |model-fallback: )")
# Two-line footers: the label names what the next line is (a model name, a
# cost), and neither line is the answer.
paired = ("served model", "configured model", "cost usd", "tokens used")
while kept:
    if not kept[-1].strip() or footer.match(kept[-1]):
        kept.pop()
    elif len(kept) >= 2 and kept[-2].strip() in paired:
        kept.pop()
        kept.pop()
    else:
        break
print("\n".join(kept))
PY
  then
    echo "error: could not extract an intent spec (artifact kept: $answer_artifact)" >&2
    intent_draft_artifact_pointers "$first_artifact" "$fallback_artifact"
    exit 3
  fi

  if ! validate_intent_file "$body" >/dev/null; then
    echo "error: the drafted spec failed validation (artifact kept: $answer_artifact)" >&2
    intent_draft_artifact_pointers "$first_artifact" "$fallback_artifact"
    exit 3
  fi

  mkdir -p "$INTENT_DIR"
  agent_memory_ensure_oms_ignore "$REPO"
  artifact_ref="$(intent_artifact_ref "$answer_artifact")"
  artifact_ref="${artifact_ref//$'\r'/}"
  first_artifact_ref=""
  if [ -n "$fallback_artifact" ]; then
    first_artifact_ref="$(intent_artifact_ref "$first_artifact")"
    first_artifact_ref="${first_artifact_ref//$'\r'/}"
  fi
  ts="$(date -u +%Y%m%dT%H%M%SZ)"
  intent_name="intent-$ts"
  target="$INTENT_DIR/$intent_name.md"
  {
    printf '## Provenance\n\n'
    printf -- '- Drafted by: %s (intent.sh; content below is model output — review before adopt)\n' "$answering_provider"
    printf -- '- Drafted at: %s\n' "$ts"
    printf -- '- Goal sentence: %s\n' "$GOAL_TEXT"
    printf -- '- Artifact: %s\n' "$artifact_ref"
    if [ -n "$fallback_artifact" ]; then
      printf -- '- First call artifact: %s\n' "$first_artifact_ref"
    fi
    printf '\n'
    cat "$body"
    printf '\n'
  } > "$target"
  rm -f "$body"
  body=""
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

  intent_snapshot="$(agent_memory_mktemp)" || fail "could not allocate intent snapshot"
  live_snapshot="$(agent_memory_mktemp)" || {
    rm -f "$intent_snapshot"
    fail "could not allocate live intent snapshot"
  }
  staged_spec="$(agent_memory_mktemp_beside "$SPEC")" || {
    rm -f "$intent_snapshot" "$live_snapshot"
    fail "could not stage PROJECT.md beside its publication path"
  }
  intent_adopt_cleanup() {
    rm -f "$intent_snapshot" "$live_snapshot" "$staged_spec"
  }
  trap intent_adopt_cleanup EXIT

  candidate_sha="$(snapshot_intent_file "$INTENT_FILE" "$intent_snapshot")" || exit 3
  candidate_sha="${candidate_sha//$'\r'/}"
  [ -n "$candidate_sha" ] || fail "could not digest the intent candidate snapshot"

  meta="$(validate_intent_file "$intent_snapshot")" || exit 3
  meta="${meta//$'\r'/}"
  accept_cmd="$(printf '%s\n' "$meta" | sed -n 2p)"

  # Downstream-contract preflight (field finding: a custom acceptance
  # without its check files sailed through adopt and refused at the FIRST
  # propose): the same parser plan-from-spec runs judges the candidate now.
  if ! preflight_out="$("$ROOT/scripts/plan-from-spec.sh" --repo "$REPO" \
      --validate-spec "$intent_snapshot" 2>&1)"; then
    printf '%s\n' "$preflight_out" >&2
    fail "the candidate's acceptance contract would refuse at plan-from-spec; fix it before adopting (usually '- Required check files:' naming what the checks read)"
  fi

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
  scope_list="$(scope_paths "$intent_snapshot" | tr -d '\r' | tr '\n' ',' | sed 's/,$//')"
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
  confirmed_intent_snapshot "$intent_snapshot" "$staged_spec" || exit $?

  # Serialize the only PROJECT.md publication point. A cooperative concurrent
  # adopter sees the same lock and exactly one can create the contract. The
  # live candidate is stable-read again inside that lock and content-bound to
  # the generation all gates consumed; an editor save during acceptance
  # therefore publishes neither the old nor the new generation.
  intent_publish_locked() {
    local expected_sha="$1"
    local frozen="$2"
    local live="$3"
    local staged="$4"
    local live_sha=""
    local create_rc=0

    if [ -e "$OUTER_RECEIPT" ] || [ -L "$OUTER_RECEIPT" ]; then
      fail "a live autopilot receipt appeared during adopt; PROJECT.md was not published"
    fi
    if [ -e "$SPEC" ] || [ -L "$SPEC" ]; then
      fail "PROJECT.md already exists; another adopter may have published it"
    fi
    live_sha="$(snapshot_intent_file "$INTENT_FILE" "$live")" || {
      fail "candidate changed during adopt; the edited draft was preserved and PROJECT.md was not published"
    }
    live_sha="${live_sha//$'\r'/}"
    if [ "$live_sha" != "$expected_sha" ] || ! cmp -s "$frozen" "$live"; then
      fail "candidate changed during adopt; the edited draft was preserved and PROJECT.md was not published"
    fi

    atomic_create_project "$staged" "$SPEC" || create_rc=$?
    case "$create_rc" in
      0) ;;
      17) fail "PROJECT.md already exists; another adopter may have published it" ;;
      *) fail "could not atomically publish PROJECT.md" ;;
    esac
  }

  publish_rc=0
  oms_with_file_lock "$SPEC" intent_publish_locked \
    "$candidate_sha" "$intent_snapshot" "$live_snapshot" "$staged_spec" || publish_rc=$?
  case "$publish_rc" in
    0) ;;
    75) fail "could not acquire the PROJECT.md publication lock" ;;
    *) exit "$publish_rc" ;;
  esac
  # Adopting is the day's clearest decision, and the goal sentence is the
  # operator's own words rather than anything this script composed. Passing it
  # through gives the Work Journal daily the contract a reader needs to know
  # what the commits underneath were for. Fail-open: the contract is adopted
  # either way.
  adopted_goal="$(sed -n 's/^- Goal sentence: //p' "$SPEC" | head -n 1)"
  work_journal_observe "$REPO" intent "$SPEC" \
    --source-id "$INTENT_ID" --outcome "Contract adopted: $INTENT_ID" \
    --outcome-status "confirmed" --verification-status not_applicable \
    ${adopted_goal:+--decision "adopted contract: $adopted_goal"} || true
  echo "intent: adopted $INTENT_ID -> PROJECT.md (State: confirmed)"
  echo "intent: next: oms plan-from-spec --repo $REPO   (or: oms autopilot propose ...)"
  exit 0
fi

fail "unknown action: $ACTION"
