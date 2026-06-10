#!/usr/bin/env bash
set -euo pipefail

# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib/multi-agent-common.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/multi-agent-common.sh"

MA_KIND="ask"
MA_SHOW_REPO=0
MA_QUORUM_FALLBACK="answer"
MA_DEBATE_ROLE="advisors"
MA_DEBATE_TOPIC="question"
MA_DEBATE_SECTIONS=$'Answer:\nChanged from previous round:\nRemaining disagreements:'

REPO="$PWD"
PROMPT=""
PROVIDERS="codex,claude,antigravity"
ARTIFACT_DIR=""
INCLUDE_STATUS=0
INCLUDE_DIFF=0
DEBATE=0
DRY_RUN="${OH_MY_SETTING_ASK_DRY_RUN:-0}"

usage() {
  cat <<'EOF'
Usage: multi-agent-ask.sh [options] --prompt TEXT

Ask the same question to Codex, Claude Code, and Antigravity, then persist each
answer as an artifact. Default mode is concept/question only; no repo context is
attached unless requested.

Options:
  --prompt TEXT        Question/task. Required.
  --repo PATH          Git repo for optional context. Default: current directory.
  --providers LIST     Comma list: codex,claude,antigravity. Default: all three.
  --artifact-dir PATH  Artifact directory. Default: PWD/.oms/artifacts/ask.
  --repo-context       Attach sanitized git status only.
  --diff               Attach sanitized git status and diff.
  --debate N           Add N debate rounds (1-3). Each round, every provider
                       sees the others' previous answers, critiques them, and
                       revises its own. Debate rounds exchange answers only;
                       repo context is attached to round-1 prompts only.
  --print-timeout DUR  Timeout for print mode wait. Default: 5m.
  --dry-run            Write prompts as artifacts without CLI calls.
  -h, --help           Show this help.

Environment:
  OH_MY_SETTING_ASK_DRY_RUN=1   Same as --dry-run.
  OMS_MULTI_AGENT_TIMEOUT=5m    Per-provider wall-clock timeout (GNU timeout).
  OMS_MULTI_AGENT_PRINT_TIMEOUT=5m Timeout for print mode wait (agy).
EOF
}

write_prompt() {
  local output="$1"
  local repo="$2"
  local question="$3"
  local status_file="$4"
  local diff_file="$5"

  {
    printf 'You are one of three independent advisors: Codex, Claude Code, and Antigravity.\n'
    printf 'Answer the same question from your own perspective. Do not modify files.\n'
    printf 'Prefer concrete reasoning, tradeoffs, assumptions, and actionable recommendations.\n'
    printf 'If the question is underspecified, state the key assumptions and what would change the answer.\n\n'
    printf 'Question:\n%s\n\n' "$question"
    if [ "$INCLUDE_STATUS" -eq 1 ] || [ "$INCLUDE_DIFF" -eq 1 ]; then
      printf 'Repository:\n%s\n\n' "$repo"
      printf 'Git status:\n'
      cat "$status_file"
      printf '\n'
    else
      printf 'Repository context: omitted.\n\n'
    fi
    if [ "$INCLUDE_DIFF" -eq 1 ]; then
      printf 'Diff:\n'
      cat "$diff_file"
      printf '\n'
    fi
    printf '\nReturn exactly these sections:\n'
    printf 'Answer:\n'
    printf 'Tradeoffs:\n'
    printf 'Risks:\n'
    printf 'Recommendation:\n'
  } > "$output"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --prompt)
      [ "$#" -ge 2 ] || fail "--prompt requires text"
      PROMPT="$2"
      shift 2
      ;;
    --repo)
      [ "$#" -ge 2 ] || fail "--repo requires path"
      REPO="$2"
      shift 2
      ;;
    --providers)
      [ "$#" -ge 2 ] || fail "--providers requires list"
      PROVIDERS="$2"
      shift 2
      ;;
    --artifact-dir)
      [ "$#" -ge 2 ] || fail "--artifact-dir requires path"
      ARTIFACT_DIR="$2"
      shift 2
      ;;
    --repo-context)
      INCLUDE_STATUS=1
      shift
      ;;
    --diff)
      INCLUDE_STATUS=1
      INCLUDE_DIFF=1
      shift
      ;;
    --debate)
      [ "$#" -ge 2 ] || fail "--debate requires round count"
      case "$2" in
        1|2|3) DEBATE="$2" ;;
        *) fail "--debate must be 1-3" ;;
      esac
      shift 2
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    --print-timeout)
      [ "$#" -ge 2 ] || fail "--print-timeout requires duration"
      OMS_MULTI_AGENT_PRINT_TIMEOUT="$2"
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

[ -n "$PROMPT" ] || fail "--prompt is required"
if [ "$INCLUDE_STATUS" -eq 1 ] || [ "$INCLUDE_DIFF" -eq 1 ]; then
  REPO="$(cd "$REPO" && pwd)"
  git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || fail "not a git repo: $REPO"
fi
ARTIFACT_DIR="${ARTIFACT_DIR:-$PWD/.oms/artifacts/ask}"

load_user_tool_paths
mkdir -p "$ARTIFACT_DIR"

status_file="$(mktemp)" || fail "mktemp failed"
diff_file="$(mktemp)" || fail "mktemp failed"
prompt_file="$(mktemp)" || fail "mktemp failed"
debate_dir=""
cleanup() {
  rm -f "$status_file" "$diff_file" "$prompt_file"
  if [ -n "$debate_dir" ]; then
    rm -rf "$debate_dir"
  fi
}
trap cleanup EXIT

if [ "$INCLUDE_STATUS" -eq 1 ]; then
  ma_safe_status "$REPO" > "$status_file"
else
  : > "$status_file"
fi
if [ "$INCLUDE_DIFF" -eq 1 ]; then
  if ! ma_safe_diff "$REPO" > "$diff_file"; then
    echo "external ask skipped: sensitive-looking diff content detected" >&2
    exit 3
  fi
else
  : > "$diff_file"
fi

write_prompt "$prompt_file" "$REPO" "$PROMPT" "$status_file" "$diff_file"

timestamp="$(date -u +%Y%m%dT%H%M%SZ)-$$"
slug="$(slugify "$PROMPT")"
[ -n "$slug" ] || slug="ask"
declare -a pids artifacts provider_names alive last_arts

ma_run_round1

if [ "$DEBATE" -gt 0 ]; then
  debate_dir="$(mktemp -d)" || fail "mktemp failed"
  ma_run_debate_rounds
fi

synth_file="$ARTIFACT_DIR/_synthesis-$slug-$timestamp.md"
ma_write_synthesis "$synth_file"

ma_quorum_exit
