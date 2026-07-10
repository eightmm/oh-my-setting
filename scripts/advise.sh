#!/usr/bin/env bash
set -euo pipefail

# Cross-CLI advisor pass at a decision point: compose an adversarial
# advisor prompt (decision context + unresolved fail-ledger rows) and send
# it to one other local agent CLI via agent-call.sh. Any agent can use this
# where Claude Code would consult its native advisor model.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

fail() {
  echo "error: $*" >&2
  exit 2
}

REPO="$PWD"
TO="${OMS_ADVISOR_PROVIDER:-}"
PROMPT=""
PROMPT_FILE=""
INCLUDE_FAILURES=1
PASSTHROUGH=()

usage() {
  cat <<'EOF'
Usage: advise.sh (--prompt TEXT | --prompt-file PATH) [options]

Consult an advisor agent at a decision point: before an irreversible
decision, after repeated failures, or before declaring work done. Sends an
adversarial-review prompt to one other local agent CLI (read-only pass via
agent-call.sh) and prints its verdict. Include in the prompt: the decision,
the evidence, the alternatives considered, and the planned next action.

Options:
  --prompt TEXT        Decision context to review.
  --prompt-file PATH   Decision context from a file.
  --to PROVIDER        Advisor provider: codex, claude, or antigravity.
                       Default: OMS_ADVISOR_PROVIDER, else the first
                       available provider that is not the caller (OMS_AGENT).
  --repo PATH          Repo for context and artifacts. Default: PWD.
  --no-failures        Do not attach unresolved fail-ledger rows.
  --no-memory          Do not attach shared harness memory.
  --no-task            Do not attach the active task handoff packet.
  --export-only        Write the prompt artifact without calling the CLI.
  --print-timeout DUR  Timeout for print mode wait (agy). Default: 5m.
  --dry-run            Write prompt artifact without calling the CLI.
  -h, --help           Show help.

Environment:
  OMS_ADVISOR_PROVIDER   Default advisor provider (overridden by --to).
  OMS_AGENT              Caller identity; the default advisor avoids it.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --prompt)
      [ "$#" -ge 2 ] || fail "--prompt requires text"
      PROMPT="$2"
      shift 2
      ;;
    --prompt-file)
      [ "$#" -ge 2 ] || fail "--prompt-file requires path"
      PROMPT_FILE="$2"
      shift 2
      ;;
    --to)
      [ "$#" -ge 2 ] || fail "--to requires provider"
      TO="$2"
      shift 2
      ;;
    --repo)
      [ "$#" -ge 2 ] || fail "--repo requires path"
      REPO="$2"
      shift 2
      ;;
    --no-failures)
      INCLUDE_FAILURES=0
      shift
      ;;
    --no-memory|--no-task|--no-ml-context|--export-only|--dry-run)
      PASSTHROUGH+=("$1")
      shift
      ;;
    --print-timeout)
      [ "$#" -ge 2 ] || fail "--print-timeout requires duration"
      PASSTHROUGH+=("$1" "$2")
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      if [ -z "$PROMPT" ]; then
        PROMPT="$1"
        shift
      else
        fail "unknown argument: $1"
      fi
      ;;
  esac
done

if [ -n "$PROMPT_FILE" ]; then
  [ -f "$PROMPT_FILE" ] || fail "prompt file not found: $PROMPT_FILE"
elif [ -z "$PROMPT" ]; then
  fail "--prompt or --prompt-file is required"
fi
[ -d "$REPO" ] || fail "repo not found: $REPO"
REPO="$(cd "$REPO" && pwd)"

provider_cli_available() {
  case "$1" in
    codex) command -v codex >/dev/null 2>&1 ;;
    claude) command -v claude >/dev/null 2>&1 ;;
    antigravity|agy) command -v agy >/dev/null 2>&1 ;;
    *) return 1 ;;
  esac
}

# Default advisor: the first available provider that is not the caller, so
# the advice comes from an independent model family when one is installed.
pick_advisor() {
  local caller="${OMS_AGENT:-}"
  local candidate

  for candidate in claude codex antigravity; do
    [ "$candidate" = "$caller" ] && continue
    if provider_cli_available "$candidate"; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  # Self-advice from a fresh context still beats no advice.
  if [ -n "$caller" ] && provider_cli_available "$caller"; then
    printf '%s\n' "$caller"
    return 0
  fi
  return 1
}

if [ -z "$TO" ]; then
  TO="$(pick_advisor)" || fail "no advisor provider CLI found (codex, claude, agy)"
fi

advise_tmpdir="$(mktemp -d)" || fail "mktemp failed"
trap 'rm -rf "$advise_tmpdir"' EXIT
advisor_prompt="$advise_tmpdir/advise-prompt.md"

# The first line doubles as the artifact slug source in agent-call, so lead
# with the decision summary rather than the fixed persona text.
summary="$PROMPT"
[ -n "$summary" ] || summary="$(head -c 120 "$PROMPT_FILE" | tr '\n' ' ')"

{
  printf 'Advisor request: %s\n\n' "$(printf '%s' "$summary" | head -c 120)"
  printf 'You are the advisor for another coding agent at a decision point.\n'
  printf 'Be adversarial: your job is to catch the wrong branch before it is\n'
  printf 'taken, not to validate it. The caller consults you before\n'
  printf 'irreversible decisions, after repeated failures, or before\n'
  printf 'declaring work done.\n\n'
  printf 'Respond with exactly these sections:\n'
  printf 'VERDICT: proceed | revise | stop\n'
  printf 'RISKS: flaws or risks in the plan, most severe first\n'
  printf 'MISSING: checks, evidence, or alternatives the caller has not considered\n'
  printf 'NEXT: the single next action you recommend\n\n'
  printf 'Keep it under 40 lines. If the context is too thin to judge, say\n'
  printf 'what is missing in MISSING and answer VERDICT: revise.\n\n'
  if [ "$INCLUDE_FAILURES" -eq 1 ] && [ -x "$SCRIPT_DIR/fail-ledger.sh" ]; then
    failures="$(cd "$REPO" && bash "$SCRIPT_DIR/fail-ledger.sh" list --unresolved 2>/dev/null | head -20 || true)"
    if [ -n "$failures" ]; then
      printf '## Known unresolved failures in this repo (fail-ledger)\n\n%s\n\n' "$failures"
    fi
  fi
  printf '## Decision context (from the caller)\n\n'
  if [ -n "$PROMPT_FILE" ]; then
    cat "$PROMPT_FILE"
  else
    printf '%s\n' "$PROMPT"
  fi
} > "$advisor_prompt"

# Not exec: the EXIT trap must outlive the call so the composed prompt file
# exists while agent-call reads it and the tmpdir is still cleaned up after.
status=0
bash "$SCRIPT_DIR/agent-call.sh" \
  --to "$TO" \
  --repo "$REPO" \
  --artifact-dir "$REPO/.oms/artifacts/advise" \
  --prompt-file "$advisor_prompt" \
  ${PASSTHROUGH[@]+"${PASSTHROUGH[@]}"} || status=$?
exit "$status"
