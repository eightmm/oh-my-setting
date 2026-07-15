#!/usr/bin/env bash
set -euo pipefail

# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib/peer-common.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/peer-common.sh"

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
INCLUDE_MEMORY=0
INCLUDE_TASK=0
INCLUDE_ML_CONTEXT=0
DEBATE=0
EXPORT_ONLY=0
GATE=0
VERIFY_CMD=""
NO_VERIFY=0
MODEL_CLASS=auto
MODEL=""
FALLBACK_MODEL=""
NO_MODEL_FALLBACK=0
REASONING_EFFORT=auto
DRY_RUN="${OH_MY_SETTING_REVIEW_DRY_RUN:-0}"

usage() {
  cat <<'EOF'
Usage: peer-review.sh [options] --prompt TEXT
       peer-review.sh verdicts [artifact-dir]

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
  --model-class CLASS  auto, fast, balanced, or deep.
  --model MODEL        Exact model; requires exactly one provider.
  --fallback-model M   Explicit fallback; requires exactly one provider.
  --no-model-fallback  Disable implicit class fallback.
  --reasoning-effort E auto, low, medium, or high.
  --no-diff            Do not attach git diff/status context.
  --memory             Attach shared harness memory.
  --task               Attach the active task handoff packet.
  --ml-context         Attach the compact ML context digest.
  --no-memory          Disable --memory (compatibility).
  --no-task            Disable --task (compatibility).
  --no-ml-context      Disable --ml-context (compatibility).
  --debate N           Add N debate rounds (1-3). Each round, every reviewer
                       sees the others' previous findings, critiques them, and
                       revises its own. Debate rounds exchange findings only;
                       the diff is attached to round-1 prompts only.
  --gate               Require each reviewer to end with GATE: pass or
                       GATE: fail, then print verdicts and exit with the gate
                       status. Review mode only.
  --verify CMD         Gate mode: run CMD in the repo after the reviews; a
                       non-zero exit forces the gate to fail regardless of
                       reviewer verdicts (a GATE: pass self-report cannot
                       pass a diff that fails the project's own checks).
                       Default when --gate is set and scripts/check.sh is
                       executable: "bash scripts/check.sh fast" (ml-smoke
                       with --ml when available).
  --no-verify          Gate mode: skip the default scripts/check.sh backstop.
  --export-only        Write provider prompt artifacts and do not call CLIs.
                       Use when the current agent may not send repo context to
                       another external provider. Import answers later with
                       import-agent-result.sh.
  --synthesize [P]     After provider reviews, run a synthesis pass with
                       provider P (codex|claude|antigravity). Default: claude.
  --print-timeout DUR  Timeout for print mode wait. Default: 5m.
  --dry-run            Write prompts as artifacts without CLI calls.
  -h, --help           Show this help.

Environment:
  OH_MY_SETTING_REVIEW_DRY_RUN=1   Same as --dry-run.
  OMS_PEER_TIMEOUT=5m       Per-provider wall-clock timeout (GNU timeout).
  OMS_PEER_PRINT_TIMEOUT=5m Timeout for print mode wait (agy).
EOF
}

write_gate_instruction() {
  cat <<'EOF'

Gate verdict:
End your response with exactly one final line: GATE: pass or GATE: fail.
The final line must contain only that exact GATE text, with no punctuation or formatting.
Use GATE: pass only if this change is ready to proceed with no blocking findings.
Use GATE: fail if any blocking bug, regression, missing test, unclear contract, or unsafe operation remains.
Do not put any text after the final GATE line.
EOF
}

review_verdicts() {
  local vdir="$1"
  local forced_run_id="${2:-}"
  local latest run_id base provider suf r f verdict overall found
  # bash 3.2 has no associative arrays; track one "provider<TAB>round<TAB>file"
  # record per candidate and pick the highest round per provider at the end.
  local vrecords="" providers

  [ -d "$vdir" ] || { echo "error: no artifact dir: $vdir" >&2; exit 2; }
  if [ -n "$forced_run_id" ]; then
    run_id="$forced_run_id"
  else
    # [!_]* skips _synthesis-*; export/import handoff artifacts hold no provider
    # review to judge; artifact names are wrapper-generated slugs, so ls -t
    # parsing is safe here. "|| true": an empty dir must not trip pipefail.
    latest="$(
      ls -t "$vdir"/[!_]*.md 2>/dev/null | while IFS= read -r f; do
        case "$f" in *.export.md|*.import.md) continue ;; esac
        printf '%s\n' "$f"
        break
      done || true
    )"
    [ -n "$latest" ] || { echo "error: no review artifacts in $vdir" >&2; exit 2; }
    # Debate rounds append -rN to the run id; strip it for grouping.
    run_id="$(printf '%s' "$latest" | sed -E 's/.*-([0-9]{8}T[0-9]{6}Z-[0-9]+)(-r[0-9]+)?\.md$/\1/')"
    [ "$run_id" != "$latest" ] || { echo "error: cannot parse run id from $(basename "$latest")" >&2; exit 2; }
  fi

  echo "run: $run_id"
  # Per provider, judge the FINAL artifact: highest debate round, else base.
  for f in "$vdir"/*-"$run_id".md "$vdir"/*-"$run_id"-r[0-9]*.md; do
    [ -e "$f" ] || continue
    base="$(basename "$f")"
    # Skip run-level artifacts (_synthesis-*, _verify-*): they are not
    # provider reviews and carry no GATE verdict.
    case "$base" in _*) continue ;; esac
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
    vrecords="$vrecords$provider	$r	$f
"
  done

  overall=0
  found=0
  providers="$(printf '%s' "$vrecords" | awk -F '\t' 'NF>=3{print $1}' | sort -u)"
  for provider in $providers; do
    # Highest round wins for this provider (final debate artifact).
    f="$(printf '%s' "$vrecords" | awk -F '\t' -v p="$provider" \
      '$1==p && $2+0>=mr {mr=$2+0; file=$3} END{print file}')"
    [ -n "$f" ] || continue
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
}

# Verdict inspection for gate loops: one line per provider from the latest
# run group, with died-mid-run detection (artifact without an exit section).
if [ "${1:-}" = "verdicts" ]; then
  shift
  review_verdicts "${1:-$PWD/.oms/artifacts/review}"
fi

validate_provider_list() {
  local normalized
  normalized="$(ma_normalize_provider_list "$PROVIDERS")" || exit $?
  PROVIDERS="$normalized"
}

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
      printf 'This is an ML pre-training gate. Prioritize silent ML bugs and metric corruption:\n'
      printf -- '- Data leakage/splits: fitting or selection on val/test; group, time, seed, preprocessing, or checkpoint leakage.\n'
      printf -- '- Objective/eval: target and metric meaning, loss sign/scale/reduction/masks, eval mode, no-grad, NaN/Inf, dtype.\n'
      printf -- '- Reproducibility/distribution: seeds, versions, checkpoint symmetry, sampler.set_epoch, rank-0 effects, metric reduction.\n'
      printf -- '- Chem-bio core: scientific units, entity-aware holdouts, provenance, cheap baselines, calibration, and applicability slices; use chem-bio-ml references for the active family.\n'
      printf -- '- Molecule/protein/3D/interactions: scaffold/sequence-identity split; family/template/pose/assay leakage; stereochemistry, construct, residue, frame, invariance; report cold-drug/cold-target/cold-both.\n'
      printf -- '- reaction, generation, nucleic acid, single-cell, and knowledge-graph: template/patent/time/locus/donor/batch/inverse-edge leakage; validity, memorization, oracle, and no-change baselines.\n\n'
    fi
    ma_write_harness_context "$repo" "$INCLUDE_MEMORY" "$INCLUDE_TASK" "$INCLUDE_ML_CONTEXT"
    printf 'Question:\n%s\n\n' "$question"
    printf 'Repository:\n%s\n\n' "$(ma_repo_label "$repo")"
    if [ "$NO_DIFF" -eq 0 ]; then
      printf 'Git status:\n'
      cat "$status_file"
      if grep -q '^??' "$status_file"; then
        # Untracked files never appear in the diff; without this note an
        # all-new-file change reads as an empty diff and gets a false pass.
        printf '\nNote: untracked (??) files above are NOT in the diff; their content was not provided. If they are the subject of this review, say so instead of "No findings".\n'
      fi
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
    --no-diff)
      NO_DIFF=1
      shift
      ;;
    --ml)
      ML_PRESET=1
      INCLUDE_ML_CONTEXT=1
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
    --debate)
      [ "$#" -ge 2 ] || fail "--debate requires round count"
      case "$2" in
        1|2|3) DEBATE="$2" ;;
        *) fail "--debate must be 1-3" ;;
      esac
      shift 2
      ;;
    --gate)
      GATE=1
      shift
      ;;
    --verify)
      [ "$#" -ge 2 ] || fail "--verify requires command"
      VERIFY_CMD="$2"
      shift 2
      ;;
    --no-verify)
      NO_VERIFY=1
      shift
      ;;
    --synthesize)
      SYNTHESIZE="claude"
      if [ "$#" -ge 2 ] && [ "${2#-}" = "$2" ]; then
        case "$2" in
          codex|claude|antigravity|agy)
            SYNTHESIZE="$2"
            shift
            ;;
          *) fail "--synthesize provider must be codex, claude, antigravity, or agy" ;;
        esac
      fi
      shift
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

if [ -z "$PROMPT" ] && [ "$ML_PRESET" -eq 1 ]; then
  PROMPT="Review the current diff for silent ML bugs before running training or expensive experiments."
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
if { [ -n "$MODEL" ] || [ -n "$FALLBACK_MODEL" ]; } && [ -n "$SYNTHESIZE" ]; then
  sole_provider="$(printf '%s' "$PROVIDERS" | tr -d '[:space:]')"
  [ "$sole_provider" != agy ] || sole_provider=antigravity
  [ "$SYNTHESIZE" != agy ] || SYNTHESIZE=antigravity
  [ "$sole_provider" = "$SYNTHESIZE" ] ||
    fail "--model cannot be reused by a different synthesis provider"
fi
if [ "$REASONING_EFFORT" != auto ] && {
     printf '%s' "$PROVIDERS" | tr ',' '\n' | grep -Eq '^[[:space:]]*(antigravity|agy)[[:space:]]*$' ||
     [ "$SYNTHESIZE" = antigravity ] || [ "$SYNTHESIZE" = agy ];
   }; then
  fail "explicit reasoning effort is unavailable for Antigravity; select a Low/Medium/High model variant"
fi
export OMS_MODEL_CLASS_REQUEST="$MODEL_CLASS" OMS_MODEL_EXPLICIT="$MODEL"
export OMS_MODEL_FALLBACK_EXPLICIT="$FALLBACK_MODEL" OMS_MODEL_NO_FALLBACK="$NO_MODEL_FALLBACK"
export OMS_REASONING_EFFORT_REQUEST="$REASONING_EFFORT"
if [ "$GATE" -eq 1 ]; then
  export MA_MODEL_OPERATION=review-gate
else
  export MA_MODEL_OPERATION=review
fi
REPO="$(cd "$REPO" && pwd)"
git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || fail "not a git repo: $REPO"
if [ -n "$BASE_REF" ]; then
  git -C "$REPO" rev-parse --verify --quiet "${BASE_REF}^{commit}" >/dev/null ||
    fail "invalid --base ref: $BASE_REF"
fi
ARTIFACT_DIR="${ARTIFACT_DIR:-$REPO/.oms/artifacts/review}"

# Mechanical gate backstop: default to the project's own check contract so a
# GATE: pass self-report alone cannot pass a diff that fails the checks.
if [ "$GATE" -eq 1 ] && [ -z "$VERIFY_CMD" ] && [ "$NO_VERIFY" -eq 0 ] && [ -x "$REPO/scripts/check.sh" ]; then
  if [ "$ML_PRESET" -eq 1 ] && oms_check_sh_has_ml_smoke "$REPO/scripts/check.sh"; then
    VERIFY_CMD="bash scripts/check.sh ml-smoke"
  else
    VERIFY_CMD="bash scripts/check.sh fast"
  fi
  echo "gate auto-verify: $VERIFY_CMD (disable with --no-verify)"
fi

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

if [ "$NO_DIFF" -eq 0 ]; then
  ma_safe_status "$REPO" > "$status_file"
  diff_rc=0
  ma_safe_diff "$REPO" > "$diff_file" || diff_rc=$?
  case "$diff_rc" in
    0) ;;
    3)
      echo "external review skipped: sensitive-looking diff content detected" >&2
      exit 3
      ;;
    *)
      fail "git diff failed for $REPO"
      ;;
  esac
  if grep -q '^??' "$status_file"; then
    echo "warning: untracked files are listed in status but their content is not in the diff (git add -N <file> to include new files)" >&2
  fi
else
  : > "$diff_file"
fi

write_prompt "$prompt_file" "$REPO" "$PROMPT" "$diff_file" "$status_file"
if [ "$GATE" -eq 1 ]; then
  write_gate_instruction >> "$prompt_file"
  MA_DEBATE_GATE_INSTRUCTION="$(write_gate_instruction)"
fi

timestamp="$(date -u +%Y%m%dT%H%M%SZ)-$$"
export OMS_OPERATION_ID="${OMS_OPERATION_ID:-review-$timestamp}"
slug="$(slugify "$PROMPT")"
[ -n "$slug" ] || slug="review"
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
synth_status=0

if [ "$EXPORT_ONLY" -eq 1 ] && [ -n "$SYNTHESIZE" ]; then
  echo "export-only: synthesis provider call skipped" >&2
elif [ -n "$SYNTHESIZE" ]; then
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
    synth_status=3
    printf 'SKIPPED: outbound synthesis context contains sensitive-looking content.\n' >> "$synth_file"
    echo "warning: synthesis skipped; sensitive-looking outbound context" >&2
  else
    synth_binary="$SYNTHESIZE"
    [ "$SYNTHESIZE" = "antigravity" ] && synth_binary="agy"
    if ! command -v "$synth_binary" >/dev/null 2>&1; then
      synth_status=127
      printf 'SKIPPED: command not found: %s\n' "$synth_binary" >> "$synth_file"
      echo "warning: synthesis provider missing: $synth_binary" >&2
    else
      export OMS_MODEL_OPERATION=review-synthesis
      synth_status=0
      ma_run_routed_provider "$SYNTHESIZE" read "$synth_prompt_file" "$synth_file" "$REPO" \
        peer-review-synthesis "$REPO" "$OMS_OPERATION_ID" || synth_status=$?
      if [ "$synth_status" -eq 0 ]; then
        echo "ok: synthesis ($SYNTHESIZE)"
      else
        echo "warning: synthesis pass failed ($SYNTHESIZE, exit $synth_status)" >&2
      fi
    fi
  fi
  rm -f "$synth_prompt_file"
fi

ma_append_artifact_index "$REPO" review-synthesis "${SYNTHESIZE:-local}" \
  "$synth_status" "$synth_file" || true

if [ "$EXPORT_ONLY" -eq 1 ]; then
  echo "summary: exported $total provider prompt(s)"
  echo "artifacts: $ARTIFACT_DIR"
  echo "synthesis: $synth_file"
  exit 0
fi

if [ "$GATE" -eq 1 ]; then
  ma_print_run_summary
  gate_verify_exit=0
  if [ -n "$VERIFY_CMD" ]; then
    if [ "$DRY_RUN" = "1" ]; then
      echo "gate verify: skipped (dry run)"
    else
      gate_verify_artifact="$ARTIFACT_DIR/_verify-$slug-$timestamp.md"
      {
        printf '# gate verify\n\n'
        printf -- '- command: %s\n' "$VERIFY_CMD"
        printf -- '- started: %s\n\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        printf '## Output\n\n'
      } > "$gate_verify_artifact"
      set +e
      (cd "$REPO" && run_verify_with_timeout bash -c "$VERIFY_CMD") >> "$gate_verify_artifact" 2>&1
      gate_verify_exit=$?
      set -e
      printf '\n\n## Exit\n\n%s\n' "$gate_verify_exit" >> "$gate_verify_artifact"
      ma_append_artifact_index "$REPO" review-verify local "$gate_verify_exit" "$gate_verify_artifact" "" "" "$gate_verify_exit" || true
      if [ "$gate_verify_exit" -eq 0 ]; then
        echo "gate verify: pass"
      else
        echo "gate verify: fail (exit $gate_verify_exit) -> $gate_verify_artifact"
      fi
    fi
  fi
  verdict_rc=0
  ( review_verdicts "$ARTIFACT_DIR" "$timestamp" ) || verdict_rc=$?
  # Mechanical failure beats reviewer consensus; a died provider (2) still
  # takes precedence so the round gets re-run first.
  if [ "$verdict_rc" -eq 0 ] && [ "$gate_verify_exit" -ne 0 ]; then
    echo "gate: fail (mechanical verify failed despite reviewer pass)"
    exit 1
  fi
  exit "$verdict_rc"
fi

ma_quorum_exit
