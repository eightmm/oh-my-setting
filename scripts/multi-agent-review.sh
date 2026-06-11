#!/usr/bin/env bash
set -euo pipefail

# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib/multi-agent-common.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/multi-agent-common.sh"

MA_KIND="review"
MA_SHOW_REPO=1
MA_QUORUM_FALLBACK="review"
MA_DEBATE_ROLE="reviewers"
MA_DEBATE_TOPIC="diff"
MA_DEBATE_SECTIONS=$'Findings:\nRisks:\nMissing tests:\nRecommendation:\nChanged from previous round:\nRemaining disagreements:'

REPO="$PWD"
PROMPT=""
PROVIDERS="codex,claude,antigravity"
ARTIFACT_DIR=""
NO_DIFF=0
BASE_REF=""
SYNTHESIZE=""
ML_PRESET=0
INCLUDE_MEMORY=1
INCLUDE_TASK=1
INCLUDE_ML_CONTEXT=1
DEBATE=0
DRY_RUN="${OH_MY_SETTING_REVIEW_DRY_RUN:-0}"

usage() {
  cat <<'EOF'
Usage: multi-agent-review.sh [options] --prompt TEXT
       multi-agent-review.sh verdicts [artifact-dir]

Ask the same review question to Codex, Claude Code, and Antigravity, then persist
each answer as an artifact.

verdicts: inspect the latest review run's artifacts and print one line per
provider — pass, fail, or incomplete (artifact has no exit section, e.g. the
run died mid-flight). For --debate runs each provider's FINAL round artifact
is used. Exit 0 all pass, 1 any fail, 2 incomplete/undeterminable; 2 takes
precedence over 1 (a died provider means the round must be re-run before any
fail is meaningful). Dry-run artifacts have no exit section and therefore
always read as incomplete.

Options:
  --prompt TEXT        Review question/task. Required unless --ml is set.
  --ml                 ML preset: inject a silent-ML-bug checklist (leakage,
                       splits, loss, eval mode, reproducibility, DDP) into
                       every reviewer prompt. Intended as a pre-training gate
                       before long runs or Slurm submissions.
  --repo PATH          Git repo to review. Default: current directory.
  --base REF           Diff base ref. Default: HEAD (staged + unstaged changes).
                       Use e.g. --base origin/main for branch/PR review.
  --providers LIST     Comma list: codex,claude,antigravity. Default: all three.
  --artifact-dir PATH  Artifact directory. Default: REPO/.oms/artifacts/review.
  --no-diff            Do not attach git diff/status context.
  --no-memory          Do not attach shared harness memory.
  --no-task            Do not attach the active task handoff packet.
  --no-ml-context      Do not attach the compact ML context digest.
  --debate N           Add N debate rounds (1-3). Each round, every reviewer
                       sees the others' previous findings, critiques them, and
                       revises its own. Debate rounds exchange findings only;
                       the diff is attached to round-1 prompts only.
  --synthesize [P]     After provider reviews, run a synthesis pass with
                       provider P (codex|claude|antigravity). Default: claude.
  --print-timeout DUR  Timeout for print mode wait. Default: 5m.
  --dry-run            Write prompts as artifacts without CLI calls.
  -h, --help           Show this help.

Environment:
  OH_MY_SETTING_REVIEW_DRY_RUN=1   Same as --dry-run.
  OMS_MULTI_AGENT_TIMEOUT=5m       Per-provider wall-clock timeout (GNU timeout).
  OMS_MULTI_AGENT_PRINT_TIMEOUT=5m Timeout for print mode wait (agy).
EOF
}

# Verdict inspection for gate loops: one line per provider from the latest
# run group, with died-mid-run detection (artifact without an exit section).
if [ "${1:-}" = "verdicts" ]; then
  shift
  vdir="${1:-$PWD/.oms/artifacts/review}"
  [ -d "$vdir" ] || { echo "error: no artifact dir: $vdir" >&2; exit 2; }
  # "|| true" inside: head's early exit must not wipe the captured output
  # under pipefail. [!_]* skips _synthesis-*; artifact names are
  # wrapper-generated slugs, so ls -t parsing is safe here.
  latest="$(ls -t "$vdir"/[!_]*.md 2>/dev/null | head -n 1 || true)"
  [ -n "$latest" ] || { echo "error: no review artifacts in $vdir" >&2; exit 2; }
  # Debate rounds append -rN to the run id; strip it for grouping.
  run_id="$(printf '%s' "$latest" | sed -E 's/.*-([0-9]{8}T[0-9]{6}Z-[0-9]+)(-r[0-9]+)?\.md$/\1/')"
  [ "$run_id" != "$latest" ] || { echo "error: cannot parse run id from $(basename "$latest")" >&2; exit 2; }

  echo "run: $run_id"
  # Per provider, judge the FINAL artifact: highest debate round, else base.
  declare -A vfile vround
  for f in "$vdir"/*-"$run_id".md "$vdir"/*-"$run_id"-r[0-9]*.md; do
    [ -e "$f" ] || continue
    base="$(basename "$f")"
    case "$base" in _synthesis-*) continue ;; esac
    provider="${base%%-*}"
    # Round suffix sits strictly after the run id ("-rN.md"); a slug that
    # happens to contain "-r2" must not be parsed as a round.
    suf="${base#*-"$run_id"}"
    r=0
    case "$suf" in
      -r[0-9]*.md)
        r="${suf#-r}"
        r="${r%.md}"
        case "$r" in *[!0-9]*) r=0 ;; esac
        ;;
    esac
    if [ -z "${vround[$provider]:-}" ] || [ "$r" -gt "${vround[$provider]}" ]; then
      vround[$provider]="$r"
      vfile[$provider]="$f"
    fi
  done

  overall=0
  found=0
  for provider in $(printf '%s\n' "${!vfile[@]}" | sort); do
    f="${vfile[$provider]}"
    found=$((found + 1))
    if ! grep -q '^## Exit' "$f"; then
      echo "$provider: incomplete (no exit section; run likely died — re-run the review)"
      overall=2
      continue
    fi
    # Verdict must be on its own line; a prompt echo inside the transcript
    # ("...exactly one line: GATE: pass or GATE: fail.") must not match.
    verdict="$(awk '/^## Output$/{o=1;next} /^## Exit$/{o=0} o' "$f" |
      grep -E '^[*[:space:]]*GATE: (pass|fail)[*[:space:]]*$' | tail -n 1 | grep -oE 'pass|fail')" || verdict=""
    case "$verdict" in
      pass) echo "$provider: pass" ;;
      fail)
        echo "$provider: fail"
        if [ "$overall" -ne 2 ]; then overall=1; fi
        ;;
      *)
        echo "$provider: no-verdict (complete but no GATE line)"
        overall=2
        ;;
    esac
  done
  [ "$found" -gt 0 ] || { echo "error: no provider artifacts for run $run_id" >&2; exit 2; }
  exit "$overall"
fi

write_prompt() {
  local output="$1"
  local repo="$2"
  local question="$3"
  local diff_file="$4"
  local status_file="$5"

  {
    printf 'You are one of three independent reviewers: Codex, Claude Code, and Antigravity.\n'
    printf 'Answer the same question from your own perspective. Do not modify files.\n'
    printf 'Find bugs, regressions, missing tests, unclear contracts, and unsafe operations.\n'
    printf 'Tie every finding to file/line evidence, diff evidence, commands, or docs.\n'
    printf 'If there are no actionable findings, say "No findings".\n\n'
    if [ "$ML_PRESET" -eq 1 ]; then
      printf 'This is an ML pre-training review. ML bugs are usually silent: the code runs,\n'
      printf 'only the metrics are wrong. Check the diff against each item:\n'
      printf -- '- Data leakage: val/test data used for fitting, normalization, feature selection, threshold tuning, checkpoint choice, or early stopping.\n'
      printf -- '- Split integrity: boundary changes, group/time leakage, seed-dependent splits.\n'
      printf -- '- Label/target or metric definition changes that silently break comparability with baselines.\n'
      printf -- '- Loss: sign, scale, reduction, masking of padded or invalid elements.\n'
      printf -- '- Eval correctness: model.eval() and torch.no_grad() in validation/inference paths.\n'
      printf -- '- Reproducibility: seeds, data versions, config capture, checkpoint save/load symmetry.\n'
      printf -- '- Silent numerics: NaN/Inf paths, division by zero, dtype/precision changes.\n'
      printf -- '- DDP: sampler.set_epoch per epoch, rank-0-only side effects, metric all_reduce.\n'
      printf -- '- Config or preprocessing changes that invalidate existing checkpoints or baselines.\n'
      printf -- '- Chem-bio (if molecular/protein data): random split instead of scaffold/sequence-identity split; near-duplicate leakage across the split; flipped or unlogged target (IC50/pIC50, Ki/Kd, ΔG sign); global metric hiding within-series ranking; missing cheap baseline.\n'
      printf 'Rank silently-wrong-metrics bugs as the highest severity findings.\n\n'
    fi
    ma_write_harness_context "$repo" "$INCLUDE_MEMORY" "$INCLUDE_TASK" "$INCLUDE_ML_CONTEXT"
    printf 'Question:\n%s\n\n' "$question"
    printf 'Repository:\n%s\n\n' "$(ma_repo_label "$repo")"
    if [ "$NO_DIFF" -eq 0 ]; then
      printf 'Git status:\n'
      cat "$status_file"
      printf '\nDiff:\n'
      cat "$diff_file"
      printf '\n'
    else
      printf 'Git context omitted by --no-diff.\n'
    fi
    printf '\nReturn exactly these sections:\n'
    printf 'Findings:\n'
    printf 'Risks:\n'
    printf 'Missing tests:\n'
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
    --base)
      [ "$#" -ge 2 ] || fail "--base requires git ref"
      BASE_REF="$2"
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
    --no-diff)
      NO_DIFF=1
      shift
      ;;
    --ml)
      ML_PRESET=1
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
    --synthesize)
      SYNTHESIZE="claude"
      if [ "$#" -ge 2 ]; then
        case "$2" in
          codex|claude|antigravity|agy)
            SYNTHESIZE="$2"
            shift
            ;;
        esac
      fi
      shift
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

if [ -z "$PROMPT" ] && [ "$ML_PRESET" -eq 1 ]; then
  PROMPT="Review the current diff for silent ML bugs before running training or expensive experiments."
fi
[ -n "$PROMPT" ] || fail "--prompt is required"
REPO="$(cd "$REPO" && pwd)"
git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || fail "not a git repo: $REPO"
if [ -n "$BASE_REF" ]; then
  git -C "$REPO" rev-parse --verify --quiet "${BASE_REF}^{commit}" >/dev/null ||
    fail "invalid --base ref: $BASE_REF"
fi
ARTIFACT_DIR="${ARTIFACT_DIR:-$REPO/.oms/artifacts/review}"

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

if [ "$NO_DIFF" -eq 0 ]; then
  ma_safe_status "$REPO" > "$status_file"
  if ! ma_safe_diff "$REPO" > "$diff_file"; then
    echo "external review skipped: sensitive-looking diff content detected" >&2
    exit 3
  fi
else
  : > "$diff_file"
fi

write_prompt "$prompt_file" "$REPO" "$PROMPT" "$diff_file" "$status_file"

timestamp="$(date -u +%Y%m%dT%H%M%SZ)-$$"
slug="$(slugify "$PROMPT")"
[ -n "$slug" ] || slug="review"
declare -a pids artifacts provider_names alive last_arts

ma_run_round1

if [ "$DEBATE" -gt 0 ]; then
  debate_dir="$(mktemp -d)" || fail "mktemp failed"
  ma_run_debate_rounds
fi

synth_file="$ARTIFACT_DIR/_synthesis-$slug-$timestamp.md"
ma_write_synthesis "$synth_file"

if [ -n "$SYNTHESIZE" ]; then
  synth_prompt_file="$(mktemp)" || fail "mktemp failed"
  {
    printf 'You are the synthesis reviewer. Below are independent reviews of the same diff.\n'
    printf 'Merge them into one verdict. Accept only findings tied to file/line, diff, command, or doc evidence.\n'
    printf 'Return exactly these sections:\n'
    printf 'Consensus:\nMust-fix:\nOptional:\nDisagreement:\nVerification:\n\n'
    cat "$synth_file"
  } > "$synth_prompt_file"

  printf '\n## Synthesis (%s)\n\n' "$SYNTHESIZE" >> "$synth_file"
  if [ "$DRY_RUN" = "1" ]; then
    printf 'DRY RUN: synthesis pass skipped.\n' >> "$synth_file"
    echo "dry-run: synthesis ($SYNTHESIZE)"
  elif ! ma_validate_outbound_prompt "$synth_prompt_file"; then
    printf 'SKIPPED: outbound synthesis context contains sensitive-looking content.\n' >> "$synth_file"
    echo "warning: synthesis skipped; sensitive-looking outbound context" >&2
  else
    synth_binary="$SYNTHESIZE"
    [ "$SYNTHESIZE" = "antigravity" ] && synth_binary="agy"
    if ! command -v "$synth_binary" >/dev/null 2>&1; then
      printf 'SKIPPED: command not found: %s\n' "$synth_binary" >> "$synth_file"
      echo "warning: synthesis provider missing: $synth_binary" >&2
    else
      set +e
      case "$SYNTHESIZE" in
        codex)
          run_with_timeout codex exec --sandbox read-only - < "$synth_prompt_file" >> "$synth_file" 2>&1
          synth_status=$?
          ;;
        claude)
          run_with_timeout claude --permission-mode plan -p < "$synth_prompt_file" >> "$synth_file" 2>&1
          synth_status=$?
          ;;
        antigravity|agy)
          run_with_timeout agy --print --sandbox --print-timeout "${OMS_MULTI_AGENT_PRINT_TIMEOUT:-5m}" < "$synth_prompt_file" >> "$synth_file" 2>&1
          synth_status=$?
          ;;
      esac
      set -e
      if [ "$synth_status" -eq 0 ]; then
        echo "ok: synthesis ($SYNTHESIZE)"
      else
        echo "warning: synthesis pass failed ($SYNTHESIZE, exit $synth_status)" >&2
      fi
    fi
  fi
  rm -f "$synth_prompt_file"
fi

ma_quorum_exit
