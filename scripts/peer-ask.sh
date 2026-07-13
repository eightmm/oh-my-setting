#!/usr/bin/env bash
set -euo pipefail

# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib/peer-common.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/peer-common.sh"

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
INCLUDE_MEMORY=1
INCLUDE_TASK=1
INCLUDE_ML_CONTEXT=1
DEBATE=0
HYPOTHESIS_PRESET=0
EXPORT_ONLY=0
MODEL_CLASS=auto
MODEL=""
FALLBACK_MODEL=""
NO_MODEL_FALLBACK=0
REASONING_EFFORT=auto
DRY_RUN="${OH_MY_SETTING_ASK_DRY_RUN:-0}"

usage() {
  cat <<'EOF'
Usage: peer-ask.sh [options] --prompt TEXT

Ask the same question to Codex, Claude Code, and Antigravity, then persist each
answer as an artifact. Default mode is concept/question only; no repo context is
attached unless requested.

Options:
  --prompt TEXT        Question/task. Required.
  --hypothesis         Pre-registration design review: inject an
                       attack-the-design checklist (falsifiability, confounds,
                       baseline fairness, split/leakage, metric fit, variance)
                       into every advisor prompt. Pass the hypothesis and the
                       planned experiment as the prompt (--prompt or positional).
                       Use before expensive runs.
  --repo PATH          Git repo for optional context. Default: current directory.
  --providers LIST     Comma list: codex,claude,antigravity. Default: all three.
  --artifact-dir PATH  Artifact directory. Default: REPO/.oms/artifacts/ask.
  --model-class CLASS  auto, fast, balanced, or deep.
  --model MODEL        Exact model; requires exactly one provider.
  --fallback-model M   Explicit fallback; requires exactly one provider.
  --no-model-fallback  Disable implicit class fallback.
  --reasoning-effort E auto, low, medium, or high.
  --repo-context       Attach sanitized git status only.
  --diff               Attach sanitized git status and diff.
  --no-memory          Do not attach shared harness memory.
  --no-task            Do not attach the active task handoff packet.
  --no-ml-context      Do not attach the compact ML context digest.
  --debate N           Add N debate rounds (1-3). Each round, every provider
                       sees the others' previous answers, critiques them, and
                       revises its own. Debate rounds exchange answers only;
                       repo context is attached to round-1 prompts only.
  --export-only        Write provider prompt artifacts and do not call CLIs.
                       Use when the current agent may not send repo context to
                       another external provider. Import answers later with
                       import-agent-result.sh.
  --print-timeout DUR  Timeout for print mode wait. Default: 5m.
  --dry-run            Write prompts as artifacts without CLI calls.
  -h, --help           Show this help.

Environment:
  OH_MY_SETTING_ASK_DRY_RUN=1   Same as --dry-run.
  OMS_PEER_TIMEOUT=5m    Per-provider wall-clock timeout (GNU timeout).
  OMS_PEER_PRINT_TIMEOUT=5m Timeout for print mode wait (agy).
EOF
}

validate_provider_list() {
  local normalized
  normalized="$(ma_normalize_provider_list "$PROVIDERS")" || exit $?
  PROVIDERS="$normalized"
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
    if [ "$HYPOTHESIS_PRESET" -eq 1 ]; then
      printf 'This is a pre-registration design review. Attack the hypothesis and the\n'
      printf 'experiment design before compute is spent. Check each item:\n'
      printf -- '- Falsifiability: what concrete observation would disprove this? If none, say so.\n'
      printf -- '- Smallest test: is there a cheaper experiment answering the same question?\n'
      printf -- '- Single variable: which variables move together? Name the confounds.\n'
      printf -- '- Baseline fairness: is the comparison against a tuned baseline on the same data and split?\n'
      printf -- '- Split/leakage: does the evaluation split actually test the claimed generalization?\n'
      printf -- '- Metric fit: does the metric measure the claim? What result would game it?\n'
      printf -- '- Variance: can one seed/run distinguish the effect from noise? How many runs are needed?\n'
      printf -- '- Prediction: is the expected direction and effect size stated BEFORE the run?\n'
      printf 'Rank "this experiment cannot falsify the hypothesis" as the most severe finding.\n\n'
    fi
    ma_write_harness_context "$repo" "$INCLUDE_MEMORY" "$INCLUDE_TASK" "$INCLUDE_ML_CONTEXT"
    printf 'Question:\n%s\n\n' "$question"
    if [ "$INCLUDE_STATUS" -eq 1 ] || [ "$INCLUDE_DIFF" -eq 1 ]; then
      printf 'Repository:\n%s\n\n' "$(ma_repo_label "$repo")"
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
    --model-class)
      [ "$#" -ge 2 ] || fail "--model-class requires value"
      MODEL_CLASS="$2"; shift 2
      ;;
    --model)
      [ "$#" -ge 2 ] || fail "--model requires value"
      MODEL="$2"; shift 2
      ;;
    --fallback-model)
      [ "$#" -ge 2 ] || fail "--fallback-model requires value"
      FALLBACK_MODEL="$2"; shift 2
      ;;
    --no-model-fallback)
      NO_MODEL_FALLBACK=1; shift
      ;;
    --reasoning-effort)
      [ "$#" -ge 2 ] || fail "--reasoning-effort requires value"
      REASONING_EFFORT="$2"; shift 2
      ;;
    --hypothesis)
      HYPOTHESIS_PRESET=1
      shift
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
    --no-memory)
      INCLUDE_MEMORY=0
      shift
      ;;
    --no-task)
      INCLUDE_TASK=0
      shift
      ;;
    --no-ml-context)
      INCLUDE_ML_CONTEXT=0
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
    --export-only)
      EXPORT_ONLY=1
      shift
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    --print-timeout)
      [ "$#" -ge 2 ] || fail "--print-timeout requires duration"
      OMS_PEER_PRINT_TIMEOUT="$2"
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

if [ -z "$PROMPT" ] && [ "$HYPOTHESIS_PRESET" -eq 1 ]; then
  fail "--hypothesis needs a prompt (--prompt or positional) with the hypothesis and the planned experiment"
fi
[ -n "$PROMPT" ] || fail "--prompt is required"
validate_provider_list
oms_model_validate_class "$MODEL_CLASS" || exit $?
oms_model_validate_name "$MODEL" || exit $?
oms_model_validate_name "$FALLBACK_MODEL" || exit $?
oms_reasoning_validate "$REASONING_EFFORT" || exit $?
if { [ -n "$MODEL" ] || [ -n "$FALLBACK_MODEL" ]; } &&
   [ "$(printf '%s' "$PROVIDERS" | tr ',' '\n' | sed '/^[[:space:]]*$/d' | wc -l | tr -d ' ')" != 1 ]; then
  fail "--model/--fallback-model requires exactly one provider"
fi
if [ "$REASONING_EFFORT" != auto ] &&
   printf '%s' "$PROVIDERS" | tr ',' '\n' | grep -Eq '^[[:space:]]*(antigravity|agy)[[:space:]]*$'; then
  fail "explicit reasoning effort is unavailable for Antigravity; select a Low/Medium/High model variant"
fi
export OMS_MODEL_CLASS_REQUEST="$MODEL_CLASS" OMS_MODEL_EXPLICIT="$MODEL"
export OMS_MODEL_FALLBACK_EXPLICIT="$FALLBACK_MODEL" OMS_MODEL_NO_FALLBACK="$NO_MODEL_FALLBACK"
export OMS_REASONING_EFFORT_REQUEST="$REASONING_EFFORT"
if [ "$HYPOTHESIS_PRESET" -eq 1 ]; then
  export MA_MODEL_OPERATION=decision
else
  export MA_MODEL_OPERATION=ask
fi
REPO="$(oms_repo_root "$REPO")" || fail "bad --repo"
if [ "$INCLUDE_STATUS" -eq 1 ] || [ "$INCLUDE_DIFF" -eq 1 ]; then
  git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || fail "not a git repo: $REPO"
fi
ARTIFACT_DIR="${ARTIFACT_DIR:-$REPO/.oms/artifacts/ask}"

load_user_tool_paths
agent_memory_ensure_oms_ignore_for_path "$ARTIFACT_DIR"
mkdir -p "$ARTIFACT_DIR"

status_file="$(mktemp)" || fail "mktemp failed"
diff_file="$(mktemp)" || fail "mktemp failed"
prompt_file="$(mktemp)" || fail "mktemp failed"
debate_dir=""
cleanup_done=0
cleanup() {
  [ "$cleanup_done" = 0 ] || return 0
  cleanup_done=1
  rm -f "$status_file" "$diff_file" "$prompt_file"
  if [ -n "$debate_dir" ]; then
    rm -rf "$debate_dir"
  fi
}
cleanup_signal() {
  local code="$1"
  trap - EXIT HUP INT TERM
  ma_kill_jobs
  cleanup
  exit "$code"
}
trap cleanup EXIT
trap 'cleanup_signal 129' HUP
trap 'cleanup_signal 130' INT
trap 'cleanup_signal 143' TERM

if [ "$INCLUDE_STATUS" -eq 1 ]; then
  ma_safe_status "$REPO" > "$status_file"
else
  : > "$status_file"
fi
if [ "$INCLUDE_DIFF" -eq 1 ]; then
  diff_rc=0
  ma_safe_diff "$REPO" > "$diff_file" || diff_rc=$?
  case "$diff_rc" in
    0) ;;
    3)
      echo "external ask skipped: sensitive-looking diff content detected" >&2
      exit 3
      ;;
    *)
      fail "git diff failed for $REPO"
      ;;
  esac
else
  : > "$diff_file"
fi

write_prompt "$prompt_file" "$REPO" "$PROMPT" "$status_file" "$diff_file"

timestamp="$(date -u +%Y%m%dT%H%M%SZ)-$$"
export OMS_OPERATION_ID="${OMS_OPERATION_ID:-ask-$timestamp}"
slug="$(slugify "$PROMPT")"
[ -n "$slug" ] || slug="ask"
declare -a pids artifacts provider_names alive last_arts

if [ "$EXPORT_ONLY" -eq 1 ]; then
  ma_export_round1
else
  ma_run_round1
fi

if [ "$EXPORT_ONLY" -eq 1 ] && [ "$DEBATE" -gt 0 ]; then
  echo "export-only: debate rounds skipped until imported answers exist" >&2
elif [ "$DEBATE" -gt 0 ]; then
  debate_dir="$(mktemp -d)" || fail "mktemp failed"
  ma_run_debate_rounds
fi

synth_file="$ARTIFACT_DIR/_synthesis-$slug-$timestamp.md"
ma_write_synthesis "$synth_file"
ma_append_artifact_index "$REPO" ask-synthesis local 0 "$synth_file" || true

if [ "$EXPORT_ONLY" -eq 1 ]; then
  echo "summary: exported $total provider prompt(s)"
  echo "artifacts: $ARTIFACT_DIR"
  echo "synthesis: $synth_file"
  exit 0
fi

ma_quorum_exit
