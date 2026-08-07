#!/usr/bin/env bash
# Globals below are consumed by functions from the sourced peer harness.
# shellcheck disable=SC2034
set -euo pipefail

# Debate rounds after the position exchange (round 2) must quote each peer's
# delta sections — not the whole revised answer — with an on-disk reference
# to the full text, and the debate must stop early when every seat declares
# "none" under "Changed from previous round:". Extraction anchors on the
# LAST header occurrence because codex-style providers echo the entire
# prompt, section headers included, inside their output stream: a
# first-occurrence match would quote the instructions as the answer.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-debate-delta.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT INT TERM HUP

# shellcheck source=../scripts/lib/peer-common.sh
. "$ROOT/scripts/lib/peer-common.sh"

fail() {
  echo "debate-delta-smoke: $*" >&2
  exit 1
}

assert_contains() {
  local file="$1"
  local text="$2"
  grep -Fq "$text" "$file" || fail "$(basename "$file") missing: $text"
}

assert_lacks() {
  local file="$1"
  local text="$2"
  if grep -Fq "$text" "$file"; then
    fail "$(basename "$file") must not contain: $text"
  fi
}

# A debate answer the way codex actually writes one: the prompt (with its
# section headers and the "none" instruction) echoed first, tool noise, then
# the real sections at the end.
write_echoing_artifact() {  # PATH MARKER CHANGED_TEXT
  local file="$1"
  local marker="$2"
  local changed="$3"
  {
    printf '# provider ask\n\n## Output\n\n'
    printf 'user\n'
    printf 'Return exactly these sections:\n'
    printf 'Answer:\nChanged from previous round:\nRemaining disagreements:\n'
    printf 'If nothing changed your position this round, write exactly "none" under "Changed from previous round:".\n'
    printf 'exec ls succeeded in 2ms\n'
    printf '%s-NOISE\n' "$marker"
    printf 'Answer:\n%s revised body line one\nline two with detail\n' "$marker"
    printf 'Changed from previous round:\n%s\n' "$changed"
    printf 'Remaining disagreements:\n%s-DISPUTE holds\n' "$marker"
    printf '\n## Exit\n\n0\n'
  } > "$file"
}

REPO="$TMP/repo"
mkdir -p "$REPO/.oms/artifacts/ask"

# --- extraction anchors on the LAST header, not the prompt echo ---
a="$REPO/.oms/artifacts/ask/codex-x-r2.md"
write_echoing_artifact "$a" ALPHA "moved to fp32 accumulation after BETA's evidence"
delta="$(ma_extract_debate_delta "$a")"
printf '%s\n' "$delta" > "$TMP/delta.txt"
assert_contains "$TMP/delta.txt" "moved to fp32 accumulation"
assert_contains "$TMP/delta.txt" "ALPHA-DISPUTE holds"
assert_lacks "$TMP/delta.txt" "write exactly"
assert_lacks "$TMP/delta.txt" "ALPHA-NOISE"
ma_extract_debate_delta "$a" >/dev/null || fail "delta extraction should succeed"

no_sections="$REPO/.oms/artifacts/ask/agy-x-r2.md"
printf '# a\n\n## Output\n\nfreeform answer only\n\n## Exit\n\n0\n' > "$no_sections"
if ma_extract_debate_delta "$no_sections" >/dev/null 2>&1; then
  fail "an answer without delta sections must not extract"
fi

# --- unchanged detection: strict, punctuation-tolerant, echo-proof ---
unchanged="$REPO/.oms/artifacts/ask/codex-unchanged.md"
write_echoing_artifact "$unchanged" GAMMA "none"
ma_debate_seat_unchanged "$unchanged" || fail "explicit none must read as unchanged"
write_echoing_artifact "$unchanged" GAMMA "None."
ma_debate_seat_unchanged "$unchanged" || fail "None. must read as unchanged"
write_echoing_artifact "$unchanged" GAMMA "dropped my S4 objection"
if ma_debate_seat_unchanged "$unchanged"; then
  fail "prose under Changed must read as changed"
fi
if ma_debate_seat_unchanged "$no_sections"; then
  fail "a sectionless answer must read as changed"
fi
# An empty Changed section must not read the disagreements below it as its
# value: nothing stated is not "none" stated.
empty_changed="$REPO/.oms/artifacts/ask/codex-empty.md"
{
  printf '# a\n\n## Output\n\nAnswer:\nbody\n'
  printf 'Changed from previous round:\n'
  printf 'Remaining disagreements:\nnone\n'
  printf '\n## Exit\n\n0\n'
} > "$empty_changed"
if ma_debate_seat_unchanged "$empty_changed"; then
  fail "an empty Changed section must read as changed"
fi

# --- round 2 quotes full answers; round 3 quotes deltas plus refs ---
PROMPT='Bound prior provider context.'
MA_DEBATE_ROLE=advisors
MA_DEBATE_TOPIC=question
MA_DEBATE_SECTIONS=$'Answer:\nChanged from previous round:\nRemaining disagreements:'

self="$REPO/.oms/artifacts/ask/codex-self.md"
other="$REPO/.oms/artifacts/ask/claude-other.md"
write_echoing_artifact "$self" SELF "self delta"
write_echoing_artifact "$other" OTHER "reworked the risk ranking"

r2="$TMP/round2.prompt"
write_debate_prompt "$r2" codex 2 "$self" "claude:$other"
assert_contains "$r2" 'OTHER-NOISE'
assert_contains "$r2" 'full answer on disk: .oms/artifacts/ask/claude-other.md'
assert_contains "$r2" 'write exactly "none" under "Changed from previous round:"'

r3="$TMP/round3.prompt"
write_debate_prompt "$r3" codex 3 "$self" "claude:$other"
assert_contains "$r3" 'reworked the risk ranking'
assert_contains "$r3" 'OTHER-DISPUTE holds'
assert_lacks "$r3" 'OTHER-NOISE'
assert_contains "$r3" 'SELF-NOISE'
assert_contains "$r3" 'full answer on disk: .oms/artifacts/ask/claude-other.md'

# A non-compliant peer falls back to the bounded full quote, and the
# fallback is named on stderr rather than silently paid for.
r3f="$TMP/round3-fallback.prompt"
err="$(write_debate_prompt "$r3f" codex 3 "$self" "claude:$no_sections" 2>&1 >/dev/null)"
assert_contains "$r3f" 'freeform answer only'
printf '%s' "$err" | grep -q 'no delta sections' ||
  fail "delta fallback must be named on stderr: $err"

# --- the loop stops when every seat declares none ---
DEBATE=3
DRY_RUN=1
slug=stable
timestamp=20260808T000000Z-1
ARTIFACT_DIR="$REPO/.oms/artifacts/ask"
debate_dir="$TMP/debate"
mkdir -p "$debate_dir"
provider_names=(codex claude)
alive=(1 1)
artifacts=("$self" "$other")
last_arts=("$self" "$other")
dropped=0
dropped_names=()

run_provider() {  # stubbed seat: answers, and never moves
  write_echoing_artifact "$3" STABLE none
}

ma_run_debate_rounds
[ "${debate_stable_round:-}" = 2 ] ||
  fail "unanimous none must stop the debate after round 2 (got: ${debate_stable_round:-unset})"
[ -f "$ARTIFACT_DIR/codex-stable-$timestamp-r2.md" ] || fail "round 2 must have run"
if ls "$ARTIFACT_DIR"/*-r3.md >/dev/null 2>&1; then
  fail "round 3 must not run after a stable round 2"
fi

# One moving seat keeps the debate alive to its budget.
rm -f "$ARTIFACT_DIR"/*-r2.md "$ARTIFACT_DIR"/*-r3.md "$ARTIFACT_DIR"/*-r4.md
alive=(1 1)
last_arts=("$self" "$other")
dropped=0
dropped_names=()
run_provider() {
  case "$1" in
    codex) write_echoing_artifact "$3" MOVER "sharpened the S11 objection" ;;
    *) write_echoing_artifact "$3" HOLDER none ;;
  esac
}
ma_run_debate_rounds
[ -z "${debate_stable_round:-}" ] ||
  fail "a moving seat must keep the debate running (stable=$debate_stable_round)"
[ -f "$ARTIFACT_DIR/codex-stable-$timestamp-r4.md" ] || fail "budgeted rounds must all run"

echo "debate-delta-smoke: ok"
