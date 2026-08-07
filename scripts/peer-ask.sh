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
THREAD_ID=""
PROVIDERS="codex,claude,antigravity"
ARTIFACT_DIR=""
INCLUDE_STATUS=0
INCLUDE_DIFF=0
INCLUDE_MEMORY=0
INCLUDE_TASK=0
INCLUDE_ML_CONTEXT=0
DEBATE=0
HYPOTHESIS_PRESET=0
EXPORT_ONLY=0
MODEL=""
FALLBACK_MODEL=""
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
                       An entry may carry a model (codex:model=NAME) to pin it.
                       Answers from one provider share a model family, which
                       the reported family count makes explicit.
  --artifact-dir PATH  Artifact directory. Default: REPO/.oms/artifacts/ask.
  --model MODEL        Exact model; requires exactly one provider.
  --fallback-model M   Explicit fallback; requires exactly one provider.
  --reasoning-effort E auto, low, medium, or high.
  --repo-context       Attach sanitized git status only.
  --diff               Attach sanitized git status and diff.
  --memory             Attach shared harness memory.
  --task               Attach the active task handoff packet.
  --ml-context         Attach the compact ML context digest.
  --no-memory          Disable --memory (compatibility).
  --no-task            Disable --task (compatibility).
  --no-ml-context      Disable --ml-context (compatibility).
  --thread ID          Record every answer in a cross-agent thread and give the
                       providers the conversation so far (agent-thread.sh).
  --debate N           Add N debate rounds (1-3). Each round, every provider
                       sees the others' previous answers, critiques them, and
                       revises its own. Debate rounds exchange answers only;
                       repo context is attached to round-1 prompts only. Full
                       positions cross once (round 2); later rounds quote only
                       each peer's delta sections plus an on-disk reference to
                       the full answer, and the debate stops early when every
                       seat declares "none" under "Changed from previous
                       round:" — stability, not consensus: recorded
                       disagreements stand.
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
  local expanded
  # Tiers first, then normalize: expansion produces the targets, normalization
  # canonicalizes the agy alias and rejects the same target twice.
  expanded="$(ma_expand_targets "$PROVIDERS")" || exit $?
  normalized="$(ma_normalize_provider_list "$expanded")" || exit $?
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
    ma_write_harness_context "$repo" "$INCLUDE_MEMORY" "$INCLUDE_TASK" "$INCLUDE_ML_CONTEXT" "$question"
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
    --model)
      [ "$#" -ge 2 ] || fail "--model requires value"
      MODEL="$2"; shift 2
      ;;
    --fallback-model)
      [ "$#" -ge 2 ] || fail "--fallback-model requires value"
      FALLBACK_MODEL="$2"; shift 2
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
    --memory)
      INCLUDE_MEMORY=1
      shift
      ;;
    --no-task)
      INCLUDE_TASK=0
      shift
      ;;
    --task)
      INCLUDE_TASK=1
      shift
      ;;
    --no-ml-context)
      INCLUDE_ML_CONTEXT=0
      shift
      ;;
    --ml-context)
      INCLUDE_ML_CONTEXT=1
      shift
      ;;
    --thread)
      [ "$#" -ge 2 ] || fail "--thread requires an id"
      THREAD_ID="$2"
      shift 2
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
oms_model_validate_name "$MODEL" || exit $?
oms_model_validate_name "$FALLBACK_MODEL" || exit $?
oms_reasoning_validate "$REASONING_EFFORT" || exit $?
if { [ -n "$MODEL" ] || [ -n "$FALLBACK_MODEL" ]; } &&
   [ "$(printf '%s\n' "$PROVIDERS" | tr ',' '\n' | sed '/^[[:space:]]*$/d' | wc -l | tr -d ' ')" != 1 ]; then
  fail "--model/--fallback-model requires exactly one provider"
fi
# A council runs one question at one effort, so every member has to accept it.
# Which members can is a capability question, not a fixed list of provider
# names: the scales differ (claude reaches xhigh and max, the others stop at
# high) and they move as the CLIs are updated.
if [ "$REASONING_EFFORT" != auto ]; then
  for ask_provider in $(printf '%s' "$PROVIDERS" | tr ',' ' '); do
    oms_reasoning_provider_validate "$ask_provider" "$REASONING_EFFORT" || exit $?
  done
fi
export OMS_MODEL_EXPLICIT="$MODEL"
export OMS_MODEL_FALLBACK_EXPLICIT="$FALLBACK_MODEL"
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
if [ -n "$THREAD_ID" ]; then
  thread_prompt="$(agent_memory_mktemp)" || thread_prompt=""
  if [ -n "$thread_prompt" ]; then
    { ma_write_thread_context "$REPO" "$THREAD_ID"; cat "$prompt_file"; } > "$thread_prompt"
    mv "$thread_prompt" "$prompt_file"
  fi
fi

timestamp="$(date -u +%Y%m%dT%H%M%SZ)-$$"
export OMS_OPERATION_ID="${OMS_OPERATION_ID:-ask-$timestamp}"
slug="$(slugify "$PROMPT")"
[ -n "$slug" ] || slug="ask"
declare -a pids artifacts provider_names alive last_arts seat_quality seat_exit

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

# One question, one turn per provider: the council becomes part of the same
# conversation instead of a separate pile of artifacts.
if [ -n "$THREAD_ID" ] && [ "$EXPORT_ONLY" -eq 0 ]; then
  ask_turn="$(agent_memory_mktemp)" || ask_turn=""
  if [ -n "$ask_turn" ]; then
    printf '%s\n' "$PROMPT" > "$ask_turn"
    ma_thread_append "$REPO" "$THREAD_ID" question "$ask_turn"
    for ask_i in "${!provider_names[@]}"; do
      if [ "${alive[ask_i]}" = 1 ]; then
        extract_output "${last_arts[ask_i]}" | ma_sanitize_quoted_output > "$ask_turn" 2>/dev/null || true
        ma_thread_append "$REPO" "$THREAD_ID" answer "$ask_turn" \
          "${provider_names[ask_i]}" "" "${last_arts[ask_i]}"
        continue
      fi
      # A seat that said nothing is named in one line rather than dropped from
      # the thread: skipping it left the conversation of record missing a seat
      # that the artifacts and the summary both know about.
      # Only these two populations qualify. A seat dropped during debate also
      # has alive=0, but it did answer round 1 and that answer is in the
      # synthesis, so calling it a non-answer here would be the opposite lie.
      if [ -n "${seat_quality[ask_i]:-}" ]; then
        ma_thread_append_nonanswer "$REPO" "$THREAD_ID" "${provider_names[ask_i]}" \
          "${seat_quality[ask_i]}" "${artifacts[ask_i]}" "${seat_quality[ask_i]}"
      elif [ "${seat_exit[ask_i]:-0}" != 0 ]; then
        ma_thread_append_nonanswer "$REPO" "$THREAD_ID" "${provider_names[ask_i]}" \
          "exit ${seat_exit[ask_i]}" "${artifacts[ask_i]}"
      fi
    done
    rm -f "$ask_turn"
  fi
fi
ma_append_artifact_index "$REPO" ask-synthesis local 0 "$synth_file" || true

if [ "$EXPORT_ONLY" -eq 1 ]; then
  echo "summary: exported $total provider prompt(s)"
  echo "artifacts: $ARTIFACT_DIR"
  echo "synthesis: $synth_file"
  exit 0
fi

ma_quorum_exit
