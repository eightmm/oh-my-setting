#!/usr/bin/env bash
set -euo pipefail

# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib/peer-common.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/peer-common.sh"
# shellcheck source=lib/work-journal.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/work-journal.sh"

MA_KIND="review"
MA_SHOW_REPO=1
MA_QUORUM_FALLBACK="review"
MA_DEBATE_ROLE="reviewers"
MA_DEBATE_TOPIC="diff"
MA_DEBATE_SECTIONS=$'Findings:\nRisks:\nMissing tests:\nRecommendation:\nChanged from previous round:\nRemaining disagreements:'

REPO="$PWD"
PROMPT=""
PROVIDERS="codex,claude,antigravity"
PROVIDERS_EXPLICIT=0
WRITER=""
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
COVERS=""
VERIFY_CMD=""
NO_VERIFY=0
MODEL=""
FALLBACK_MODEL=""
REASONING_EFFORT=auto
DRY_RUN="${OH_MY_SETTING_REVIEW_DRY_RUN:-0}"

usage() {
  cat <<'EOF'
Usage: peer-review.sh [options] --prompt TEXT
       peer-review.sh verdicts [--json] [artifact-dir]

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
                       An entry may carry a model (codex:model=NAME) to pin it.
  --writer PROVIDER    Name the provider that authored the change under
                       review. By default that provider is dropped from the
                       reviewer council (its family re-judging its own patch
                       is correlated judgment, not independence); an explicit
                       --providers list including it is honored with a
                       warning.
  --artifact-dir PATH  Artifact directory. Default: REPO/.oms/artifacts/review.
  --model MODEL        Exact model; requires exactly one provider.
  --fallback-model M   One-shot capacity fallback; requires one provider.
  --reasoning-effort E auto, low, medium, high, xhigh, max, or ultra.
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
                       the diff is attached to round-1 prompts only. Full
                       findings cross once (round 2); later rounds quote only
                       each reviewer's delta sections plus an on-disk
                       reference to the full answer, and the debate stops
                       early when every seat declares "none" under "Changed
                       from previous round:".
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
  --covers ID          Gate mode: acceptance criterion the gate verify run
                       proves (repeatable). Ids are validated against the
                       runtime envelope up front and ride the mechanical
                       verify receipt exit-judged; reviewer prose never
                       contributes verified coverage, so --covers requires
                       a gate verify command.
  --no-verify          Gate mode: skip the default scripts/check.sh backstop.
  --export-only        Write provider prompt artifacts and do not call CLIs.
                       Use when the current agent may not send repo context to
                       another external provider. Import answers later with
                       `oms artifact-index import`.
  --synthesize [P]     After provider reviews, run a synthesis pass with
                       provider P (codex|claude|antigravity). Default: claude.
  --print-timeout DUR  Timeout for print mode wait. Default: 5m.
  --dry-run            Write prompts as artifacts without CLI calls.
  -h, --help           Show this help.

Environment:
  OH_MY_SETTING_REVIEW_DRY_RUN=1   Same as --dry-run.
  OMS_PEER_TIMEOUT          Per-provider wall-clock timeout (default: 20m).
  OMS_PEER_PRINT_TIMEOUT    Timeout for print mode wait (agy); tracks the
                            verb wall default unless set.
EOF
}

write_gate_instruction() {
  cat <<'EOF'

Gate verdict:
Immediately before the final line, emit one line: CONFIDENCE: <value between 0.0 and 1.0>, your calibrated confidence in the verdict.
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
  local json_out="${3:-}"
  local json_stdout="${4:-0}"
  local latest run_id base provider suf r f verdict overall found reason
  local requested_model selected_model model_family
  # bash 3.2 has no associative arrays; track one "provider<TAB>round<TAB>file"
  # record per candidate and pick the highest round per provider at the end.
  local vrecords="" providers
  # One extraction pass feeds prose, JSON, and the typed outcome row alike:
  # a second parser is where verdict drift starts.
  local seat_rows="" seat_round exit_val died_exit died_stop

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

  [ "$json_stdout" = 1 ] || echo "run: $run_id"
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
    seat_round="$(printf '%s' "$vrecords" | awk -F '\t' -v p="$provider" \
      '$1==p && $2+0>=mr {mr=$2+0} END{print mr+0}')"
    found=$((found + 1))
    if ! grep -q '^## Exit' "$f"; then
      [ "$json_stdout" = 1 ] ||
        echo "$provider: incomplete (no exit section; run likely died — re-run the review)"
      seat_rows="$seat_rows$provider	incomplete		$seat_round	$(basename "$f")
"
      overall=2
      continue
    fi
    # Verdict must be on its own line; a prompt echo inside the transcript
    # ("...exactly one line: GATE: pass or GATE: fail.") must not match.
    verdict="$(awk '/^## Output$/{o=1;next} /^## Exit$/{o=0} o' "$f" |
      grep -E '^[*[:space:]]*GATE: (pass|fail)[*[:space:]]*$' | tail -n 1 | grep -oE 'pass|fail')" || verdict=""
    # The recorded exit is part of the verdict: a seat that died nonzero
    # (timeout, kill, provider error) left a partial transcript, and a GATE
    # line inside it — printed before the death — is not a judgment of the
    # whole diff. Blank it so the no-verdict branch fails the gate closed.
    # Exits that never ran the model (blocked, missing CLI) have no GATE line
    # and keep their own diagnosis below. The last exit section is the
    # harness-written one.
    died_exit=""
    exit_val="$(awk '/^## Exit$/{e=1; v=""; next} e && NF {v=$0; e=0} END{print v}' "$f" |
      tr -d '[:space:]')"
    if [ -n "$verdict" ] && [ -n "$exit_val" ] && [ "$exit_val" != "0" ]; then
      died_exit="$exit_val"
      verdict=""
    fi
    # A recorded stop reason outranks the text: a seat cut off at max_tokens
    # exits 0 with finished-looking sentences, and a GATE line above the cut
    # judged only what survived it. Same Output window as the GATE parse —
    # a replayed thread turn can quote a marker line into the prompt, and a
    # quoted marker is not the transport's verdict.
    died_stop="$(awk '/^## Output$/{o=1;next} /^## Exit$/{o=0}
      o && /^stop-reason: .*(reason=max_tokens|reason=stream_truncated|is_error=1)/{print substr($0, 14, 67); exit}' "$f")"
    if [ -n "$verdict" ] && [ -n "$died_stop" ]; then
      verdict=""
    fi
    # Stated confidence is advisory display for tiebreaks, never a gate
    # input: a confident wrong verdict must not outvote the mechanical check.
    confidence="$(awk '/^## Output$/{o=1;next} /^## Exit$/{o=0} o' "$f" |
      grep -E '^[*[:space:]]*CONFIDENCE: (0(\.[0-9]+)?|1(\.0+)?)[*[:space:]]*$' |
      tail -n 1 | grep -oE '0(\.[0-9]+)?|1(\.0+)?' | tail -n 1)" || confidence=""
    # Provider is the transport; model is the reviewed seat. Preserve both the
    # route requested at dispatch and the final selected route after any
    # bounded fallback so downstream cross-family claims are auditable.
    requested_model="$(sed -n 's/^model-route: .* primary=\([^ ]*\) fallback=.*/\1/p' "$f" | sed -n 1p)"
    selected_model="$(sed -n \
      -e 's/^model-fallback: .* selected=\([^ ]*\).*/\1/p' \
      -e 's/^model-result: selected=\([^ ]*\) .*/\1/p' \
      -e 's/^model-result: declined by [^ ]* (\([^)]*\));.*/\1/p' \
      "$f" | tail -n 1)"
    [ -n "$selected_model" ] || selected_model="$requested_model"
    model_family="$(oms_provider_model_family "$provider" "$selected_model" 2>/dev/null || printf unknown)"
    suffix=""
    [ -z "$confidence" ] || suffix=" (confidence $confidence)"
    case "$verdict" in
      pass)
        [ "$json_stdout" = 1 ] || echo "$provider: pass$suffix"
        seat_rows="$seat_rows$provider	pass	$confidence	$seat_round	$(basename "$f")		$requested_model	$selected_model	$model_family
"
        ;;
      fail)
        [ "$json_stdout" = 1 ] || echo "$provider: fail$suffix"
        seat_rows="$seat_rows$provider	fail	$confidence	$seat_round	$(basename "$f")		$requested_model	$selected_model	$model_family
"
        if [ "$overall" -ne 2 ]; then overall=1; fi
        ;;
      *)
        # A provider whose permission was denied also finishes with no GATE
        # line, and both readings fail the gate. Only one of them is fixed by
        # editing an allow-list, so the operator has to be told which this is.
        # No DRY_RUN guard is needed: a dry-run artifact has no exit section and
        # is caught above as incomplete, so it never reaches this branch.
        reason=""
        [ "${OMS_COUNCIL_QUALITY:-1}" = "0" ] ||
          [ "$(ma_answer_quality "$f")" != "blocked" ] ||
          reason="$(ma_answer_block_reason "$f")"
        if [ -n "$reason" ]; then
          [ "$json_stdout" = 1 ] || echo "$provider: no-verdict (blocked: $reason)"
        elif [ -n "$died_exit" ] || [ -n "$died_stop" ]; then
          [ "$json_stdout" = 1 ] ||
            echo "$provider: no-verdict (${died_exit:+provider exited $died_exit}${died_stop:+incomplete answer: $died_stop}; a GATE line in a partial transcript is not a verdict — re-run the review)"
        else
          [ "$json_stdout" = 1 ] || echo "$provider: no-verdict (complete but no GATE line)"
        fi
        seat_rows="$seat_rows$provider	no-verdict		$seat_round	$(basename "$f")	$(printf '%s' "$reason" | tr '\t\n' '  ' | cut -c1-200)	$requested_model	$selected_model	$model_family
"
        overall=2
        ;;
    esac
  done
  [ "$found" -gt 0 ] || { echo "error: no provider artifacts for run $run_id" >&2; exit 2; }
  if [ -n "$json_out" ] || [ "$json_stdout" = 1 ]; then
    # Rows travel via the environment, never interpolated into the python
    # source: block reasons quote untrusted provider output.
    if ! OMS_VERDICTS_RUN="$run_id" OMS_VERDICTS_OVERALL="$overall" \
      OMS_VERDICTS_JSON_OUT="$json_out" OMS_VERDICTS_ROWS="$seat_rows" python3 - <<'PY'
import json, os, sys

rows = os.environ.get("OMS_VERDICTS_ROWS", "")
seats = []
for line in rows.splitlines():
    if not line.strip():
        continue
    parts = (line.split("\t") + [""] * 9)[:9]
    provider, verdict, confidence, rnd, artifact, reason, requested, selected, family = parts
    seat = {
        "provider": provider,
        "verdict": verdict,
        "confidence": float(confidence) if confidence else None,
        "round": int(rnd) if rnd.isdigit() else 0,
        "artifact": artifact,
    }
    if reason:
        seat["blocked_reason"] = reason
    if requested:
        seat["requested_model"] = requested
    if selected:
        seat["selected_model"] = selected
    if family:
        seat["model_family"] = family
    seats.append(seat)
data = {
    "run": os.environ["OMS_VERDICTS_RUN"],
    "overall": int(os.environ["OMS_VERDICTS_OVERALL"]),
    "seats": seats,
}
out = os.environ.get("OMS_VERDICTS_JSON_OUT", "")
text = json.dumps(data, ensure_ascii=False, separators=(",", ":"))
if out:
    with open(out, "w", encoding="utf-8") as fh:
        fh.write(text + "\n")
else:
    print(text)
PY
    then
      echo "error: could not serialize the typed verdicts; failing closed" >&2
      exit 2
    fi
  fi
  exit "$overall"
}

# Verdict inspection for gate loops: one line per provider from the latest
# run group, with died-mid-run detection (artifact without an exit section).
# --json emits the same collector's output as one JSON object for machine
# consumers; the exit contract (0/1/2) is identical in both modes.
if [ "${1:-}" = "verdicts" ]; then
  shift
  VERDICTS_JSON_STDOUT=0
  if [ "${1:-}" = "--json" ]; then
    VERDICTS_JSON_STDOUT=1
    shift
  fi
  review_verdicts "${1:-$PWD/.oms/artifacts/review}" "" "" "$VERDICTS_JSON_STDOUT"
fi

validate_provider_list() {
  local normalized
  local expanded
  local writer_norm
  local writer_provider
  local kept
  local entry
  local -a entries
  # Tiers first, then normalize: expansion produces the targets, normalization
  # canonicalizes the agy alias and rejects the same target twice.
  expanded="$(ma_expand_targets "$PROVIDERS")" || exit $?
  normalized="$(ma_normalize_provider_list "$expanded")" || exit $?
  PROVIDERS="$normalized"
  # The patch author's family re-judging its own patch is correlated
  # judgment, not independence. Unless the caller pinned --providers, the
  # writer sits out of the council; an explicit list is honored but named.
  if [ -n "$WRITER" ]; then
    writer_norm="$(ma_normalize_provider_list "$WRITER")" || exit $?
    case "$writer_norm" in
      *,*) fail "--writer takes exactly one provider" ;;
    esac
    writer_provider="${writer_norm%%:*}"
    IFS=',' read -r -a entries <<< "$PROVIDERS"
    if [ "$PROVIDERS_EXPLICIT" -eq 1 ]; then
      for entry in "${entries[@]}"; do
        if [ "${entry%%:*}" = "$writer_provider" ]; then
          echo "warning: writer $writer_provider sits in its own review council — family independence reduced" >&2
          break
        fi
      done
    else
      kept=""
      for entry in "${entries[@]}"; do
        [ "${entry%%:*}" = "$writer_provider" ] && continue
        kept="${kept:+$kept,}$entry"
      done
      [ -n "$kept" ] || fail "--writer $writer_provider leaves no reviewers; pass --providers explicitly"
      PROVIDERS="$kept"
      echo "writer: $writer_provider (excluded from reviewer council)"
    fi
  fi
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
      printf '\n'
    fi
    ma_write_harness_context "$repo" "$INCLUDE_MEMORY" "$INCLUDE_TASK" "$INCLUDE_ML_CONTEXT" "$question"
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
      PROVIDERS_EXPLICIT=1
      shift 2
      ;;
    --writer)
      [ "$#" -ge 2 ] || fail "--writer requires a provider"
      WRITER="$2"
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
    --covers)
      [ "$#" -ge 2 ] || fail "--covers requires a criterion id"
      COVERS="$COVERS $2"
      shift 2
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
export OMS_MODEL_EXPLICIT="$MODEL"
export OMS_MODEL_FALLBACK_EXPLICIT="$FALLBACK_MODEL"
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
  elif oms_check_sh_has_fast_mode "$REPO/scripts/check.sh"; then
    VERIFY_CMD="bash scripts/check.sh fast"
  else
    VERIFY_CMD="bash scripts/check.sh"
  fi
  echo "gate auto-verify: $VERIFY_CMD (disable with --no-verify)"
fi

# Explicit coverage may only ride the mechanical verify receipt: reviewer
# prose is advisory and never projects a criterion to verified. Both the
# id set and the presence of a verify command are checked before any
# reviewer is called or any artifact is written.
if [ -n "$COVERS" ]; then
  [ "$GATE" -eq 1 ] || fail "--covers requires --gate"
  [ -n "$VERIFY_CMD" ] ||
    fail "--covers requires a gate verify command (pass --verify or drop --no-verify)"
  ma_validate_covers_ids "$REPO" "$COVERS" || exit 2
fi

load_user_tool_paths
status_file="$(mktemp)" || fail "mktemp failed"
diff_file="$(mktemp)" || fail "mktemp failed"
raw_diff_file="$(mktemp)" || fail "mktemp failed"
prompt_file="$(mktemp)" || fail "mktemp failed"
gate_verify_temp="$(mktemp)" || fail "mktemp failed"
debate_dir=""
cleanup_done=0
cleanup() {
  [ "$cleanup_done" = 0 ] || return 0
  cleanup_done=1
  rm -f "$status_file" "$diff_file" "$raw_diff_file" "$prompt_file" "$gate_verify_temp"
  if [ -n "$debate_dir" ]; then
    rm -rf "$debate_dir"
  fi
}
cleanup_signal() {
  local code="$1"
  local direct_jobs parent_pgid job member pgid groups="" state tries
  trap - EXIT HUP INT TERM

  # Capture provider-created process groups before terminating the direct jobs.
  # Most non-interactive shells keep children in our group (which must never be
  # signalled as a whole), but CLIs may create their own session/group and then
  # spawn grandchildren. Kill those groups as well as the recursively found
  # PIDs so a timeout helper cannot leave the real model process orphaned.
  direct_jobs="$(jobs -pr || true)"
  parent_pgid="$(ps -o pgid= -p $$ 2>/dev/null | tr -d '[:space:]' || true)"
  case "$parent_pgid" in ''|*[!0-9]*) parent_pgid="" ;; esac
  for job in $direct_jobs; do
    for member in $job $(ma_descendant_pids "$job" || true); do
      pgid="$(ps -o pgid= -p "$member" 2>/dev/null | tr -d '[:space:]' || true)"
      case "$pgid" in ''|*[!0-9]*) continue ;; esac
      [ -n "$parent_pgid" ] || continue
      [ "$pgid" != "$parent_pgid" ] || continue
      case " $groups " in *" $pgid "*) ;; *) groups="$groups $pgid" ;; esac
    done
  done
  for pgid in $groups; do kill -TERM "-$pgid" 2>/dev/null || true; done
  ma_kill_jobs
  for pgid in $groups; do kill -KILL "-$pgid" 2>/dev/null || true; done

  # Reap direct children within a fixed one-second bound. A zombie/absent PID
  # is safe to wait immediately; an uninterruptible survivor is reported and
  # left for process teardown instead of wedging the signal handler forever.
  for job in $direct_jobs; do
    tries=0
    while kill -0 "$job" 2>/dev/null && [ "$tries" -lt 20 ]; do
      state="$(ps -o stat= -p "$job" 2>/dev/null | tr -d '[:space:]' || true)"
      case "$state" in Z*|"") break ;; esac
      sleep 0.05
      tries=$((tries + 1))
    done
    state="$(ps -o stat= -p "$job" 2>/dev/null | tr -d '[:space:]' || true)"
    case "$state" in
      Z*|"") wait "$job" 2>/dev/null || true ;;
      *) echo "warning: provider job $job did not reap before signal cleanup deadline" >&2 ;;
    esac
  done
  cleanup
  exit "$code"
}
trap cleanup EXIT
trap 'cleanup_signal 129' HUP
trap 'cleanup_signal 130' INT
trap 'cleanup_signal 143' TERM

# Run the mechanical backstop before creating any repository-local review
# artifacts or indexes. A verifier that asserts `.oms`/status purity therefore
# judges the submitted tree, not peer-review's own bookkeeping.
gate_verify_exit=0
gate_verify_ran=0
if [ "$GATE" -eq 1 ] && [ -n "$VERIFY_CMD" ]; then
  if [ "$DRY_RUN" = "1" ]; then
    echo "gate verify: skipped (dry run)"
  else
    gate_verify_ran=1
    {
      printf '# gate verify\n\n'
      printf -- '- command: %s\n' "$VERIFY_CMD"
      printf -- '- started: %s\n\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
      printf '## Output\n\n'
    } > "$gate_verify_temp"
    set +e
    (
      cd "$REPO" || exit 1
      unset OMS_MODEL_OPERATION OMS_MODEL_OPERATION_REQUEST
      unset OMS_MODEL_EXPLICIT OMS_MODEL_FALLBACK_EXPLICIT
      unset OMS_REASONING_EFFORT_REQUEST OMS_REASONING_FALLBACK_EXPLICIT
      run_verify_with_timeout bash -c "$VERIFY_CMD"
    ) >> "$gate_verify_temp" 2>&1
    gate_verify_exit=$?
    set -e
    printf '\n\n## Exit\n\n%s\n' "$gate_verify_exit" >> "$gate_verify_temp"
  fi
fi

agent_memory_ensure_oms_ignore_for_path "$ARTIFACT_DIR"
mkdir -p "$ARTIFACT_DIR"

if [ "$NO_DIFF" -eq 0 ]; then
  # External diff/textconv are mechanically disabled below. Clean filters are
  # different: Git may execute them while preparing an ordinary diff, so raw
  # config inspection rejects filters, includes, and fsmonitor first.
  oms_git_assert_safe_execution_config "$REPO" diff-read ||
    fail "unsafe Git execution config blocks review diff capture"
  ma_safe_status "$REPO" > "$status_file"
  diff_base="$(ma_git_diff_base "$REPO")"
  # The checkout is worker-writable. Never let diff.external, a per-path
  # external driver, or textconv execute under the reviewer's authority or
  # substitute forged bytes for the review/gate digest.
  GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null \
    GIT_CONFIG_NOSYSTEM=1 \
    git -c core.fsmonitor=false -c diff.external= -C "$REPO" \
    diff --no-ext-diff --no-textconv \
    "$diff_base" -- "${MA_SAFE_PATHS[@]}" > "$raw_diff_file" ||
    fail "git diff failed for $REPO"
  if contains_sensitive_content "$raw_diff_file"; then
    echo "external review skipped: sensitive-looking diff content detected" >&2
    exit 3
  fi
  diff_budget="$(ma_prompt_diff_bytes)"
  raw_diff_bytes="$(LC_ALL=C wc -c < "$raw_diff_file" | tr -d ' ')"
  if [ "$GATE" -eq 1 ] && [ "$raw_diff_bytes" -gt "$diff_budget" ]; then
    echo "error: blocking gate requires the complete diff; $raw_diff_bytes bytes exceed OMS_PROMPT_DIFF_BYTES=$diff_budget" >&2
    echo "hint: raise OMS_PROMPT_DIFF_BYTES or split the change into a fully reviewable diff" >&2
    exit 2
  fi
  ma_emit_bounded_prompt_file "$raw_diff_file" "$diff_budget" "git diff" \
    "OMS_PROMPT_DIFF_BYTES" > "$diff_file"
  if grep -q '^??' "$status_file"; then
    if [ "$GATE" -eq 1 ]; then
      echo "error: blocking gate cannot review untracked file content; add intent-to-add or stage it so the complete diff is visible" >&2
      exit 2
    fi
    echo "warning: untracked files are listed in status but their content is not in the diff (git add -N <file> to include new files)" >&2
  fi
else
  : > "$diff_file"
  : > "$raw_diff_file"
fi

write_prompt "$prompt_file" "$REPO" "$PROMPT" "$diff_file" "$status_file"
# Freeze the reviewed-diff identity now: the typed outcome row must bind the
# exact bytes the seats judged, not whatever the tree holds at exit.
REVIEW_DIFF_SHA=""
[ "$NO_DIFF" -eq 1 ] || REVIEW_DIFF_SHA="$(ma_sha256_file "$raw_diff_file" 2>/dev/null || true)"
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
    ma_answer_language_block
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

record_review_outcome() {
  local review_rc="$1"
  local verified="$2"
  local verdict_json_file="${3:-}"
  local summary status payload=""

  if [ "$review_rc" -eq 0 ]; then
    summary="Peer review outcome passed"
    status="success"
  else
    summary="Peer review outcome failed"
    status="failure"
  fi
  if [ -n "$verdict_json_file" ] && [ -s "$verdict_json_file" ]; then
    # The gate authored these verdicts itself one extraction pass ago, so a
    # payload that no longer composes is a bug, and the row must not go out
    # thin while looking authoritative: fail the gate loudly.
    payload="$(OMS_REVIEW_GATE_VERIFY_EXIT="${gate_verify_exit:-}" \
      OMS_REVIEW_DIFF_SHA256="$REVIEW_DIFF_SHA" \
      python3 - "$verdict_json_file" <<'PY'
import json, os, sys
with open(sys.argv[1], encoding="utf-8") as fh:
    data = json.load(fh)
if not isinstance(data, dict) or not isinstance(data.get("seats"), list):
    raise SystemExit("typed verdicts are not an object with seats")
gate_verify = os.environ.get("OMS_REVIEW_GATE_VERIFY_EXIT", "")
data["gate_verify_exit"] = int(gate_verify) if gate_verify.lstrip("-").isdigit() else None
diff_sha = os.environ.get("OMS_REVIEW_DIFF_SHA256", "")
if diff_sha:
    data["diff_sha256"] = diff_sha
print(json.dumps(data, ensure_ascii=False, separators=(",", ":")))
PY
)" || fail "typed review outcome could not be composed; failing closed"
    OMS_INDEX_REVIEW_OUTCOME_JSON="$payload" ma_append_artifact_index \
      "$REPO" review-outcome local "$review_rc" "$synth_file" ||
      fail "typed review outcome was not recorded; failing closed"
  else
    ma_append_artifact_index "$REPO" review-outcome local "$review_rc" "$synth_file" || true
  fi
  work_journal_observe "$REPO" peer-review "$synth_file" \
    --source-id "$OMS_OPERATION_ID:outcome" --event-type patch_review \
    --outcome "$summary" --outcome-status "$status" \
    --verification-status "$verified"
}

if [ "$GATE" -eq 1 ]; then
  ma_print_run_summary
  if [ "$gate_verify_ran" -eq 1 ]; then
      gate_verify_artifact="$ARTIFACT_DIR/_verify-$slug-$timestamp.md"
      cp "$gate_verify_temp" "$gate_verify_artifact"
      # The verify run just proved (or refuted) the exact command it ran. The
      # plan-acceptance criterion is named by that command's own digest, so the
      # coverage claim is derivable here with no side channel: if the plan's
      # acceptance command is this command, the row links; if not, the covers
      # id simply matches no criterion and stays inert.
      # The derived plan-acceptance cover is as load-bearing as an explicit
      # one: the projection trusts the latest fresh row, so a failed verify
      # receipt that silently vanished would leave an older pass speaking
      # for the acceptance command. Compose and append both fail closed.
      gate_verify_covers="$(OMS_PR_VERIFY_CMD="$VERIFY_CMD" OMS_PR_VERIFY_EXIT="$gate_verify_exit" \
        OMS_PR_COVERS_IDS="$COVERS" python3 -c '
import hashlib, json, os
command = os.environ.get("OMS_PR_VERIFY_CMD", "")
digest = hashlib.sha256(command.encode("utf-8")).hexdigest()
status = "verified" if os.environ.get("OMS_PR_VERIFY_EXIT") == "0" else "failed"
covers = ["criterion-plan-acceptance-" + digest[:10]]
for cid in os.environ.get("OMS_PR_COVERS_IDS", "").split():
    if cid and cid not in covers:
        covers.append(cid)
print(json.dumps({
    "covers": covers,
    "status": status,
}))')" || fail "could not compose the gate verify coverage payload"
      OMS_INDEX_COVERS_JSON="$gate_verify_covers" ma_append_artifact_index \
        "$REPO" review-verify local "$gate_verify_exit" "$gate_verify_artifact" "" "" "$gate_verify_exit" ||
        fail "gate verify receipt was not recorded; failing closed"
      if [ "$gate_verify_exit" -eq 0 ]; then
        echo "gate verify: pass"
      else
        echo "gate verify: fail (exit $gate_verify_exit) -> $gate_verify_artifact"
      fi
  fi
  verdict_rc=0
  verdict_json_file="$(mktemp)" || fail "mktemp failed"
  ( review_verdicts "$ARTIFACT_DIR" "$timestamp" "$verdict_json_file" ) || verdict_rc=$?
  # A structural failure exits before the collector serializes; the row then
  # goes out without a payload, which is honest. A clean pass with no typed
  # outcome is the inconsistent state that must fail closed.
  if [ "$verdict_rc" -eq 0 ] && [ ! -s "$verdict_json_file" ]; then
    echo "error: gate verdicts produced no typed outcome; failing closed" >&2
    verdict_rc=2
  fi
  # Mechanical failure beats reviewer consensus; a died provider (2) still
  # takes precedence so the round gets re-run first.
  if [ "$verdict_rc" -eq 0 ] && [ "$gate_verify_exit" -ne 0 ]; then
    echo "gate: fail (mechanical verify failed despite reviewer pass)"
    record_review_outcome 1 failed "$verdict_json_file"
    rm -f "$verdict_json_file"
    exit 1
  fi
  if [ "$verdict_rc" -eq 0 ]; then
    record_review_outcome 0 passed "$verdict_json_file"
  else
    record_review_outcome "$verdict_rc" failed "$verdict_json_file"
  fi
  rm -f "$verdict_json_file"
  exit "$verdict_rc"
fi

quorum_rc=0
( ma_quorum_exit ) || quorum_rc=$?
if [ "$quorum_rc" -eq 0 ]; then
  record_review_outcome 0 not_verified
else
  record_review_outcome "$quorum_rc" failed
fi
exit "$quorum_rc"
