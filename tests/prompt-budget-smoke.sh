#!/usr/bin/env bash
# Globals below are consumed by functions from the sourced peer harness.
# shellcheck disable=SC2034
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-prompt-budget.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT INT TERM HUP

# shellcheck source=../scripts/lib/peer-common.sh
. "$ROOT/scripts/lib/peer-common.sh"

fail() {
  echo "prompt-budget-smoke: $*" >&2
  exit 1
}

assert_bounded() {
  local file="$1"
  local budget="$2"
  local allowance=160
  local bytes
  bytes="$(LC_ALL=C wc -c < "$file" | tr -d ' ')"
  [ "$bytes" -le $((budget + allowance)) ] ||
    fail "$(basename "$file") is $bytes bytes; expected at most $((budget + allowance))"
}

assert_contains() {
  local file="$1"
  local text="$2"
  grep -Fq "$text" "$file" || fail "$(basename "$file") missing: $text"
}

write_artifact() {
  local file="$1"
  local prefix="$2"
  {
    printf '# provider ask\n\n## Output\n\n'
    printf '%s\n' "$prefix"
    awk 'BEGIN { for (i = 0; i < 24; i++) printf "evidence-%04d-abcdefghijklmnopqrstuvwxyz0123456789\n", i }'
    printf '\n## Exit\n\n0\n'
  } > "$file"
}

test_budget_defaults_and_invalid_fallback() {
  unset OMS_PROMPT_DIFF_BYTES OMS_PROMPT_QUOTE_BYTES
  [ "$(ma_prompt_diff_bytes)" = 65536 ] || fail "unexpected default diff budget"
  [ "$(ma_prompt_quote_bytes)" = 16384 ] || fail "unexpected default quote budget"
  [ "$(OMS_PROMPT_DIFF_BYTES=0 ma_prompt_diff_bytes)" = 65536 ] ||
    fail "zero diff budget should use the safe default"
  [ "$(OMS_PROMPT_QUOTE_BYTES=invalid ma_prompt_quote_bytes)" = 16384 ] ||
    fail "invalid quote budget should use the safe default"
}

test_diff_budget_and_small_passthrough() {
  local repo="$TMP/repo"
  local expected="$TMP/small-diff.expected"
  local actual="$TMP/small-diff.actual"
  local capped="$TMP/large-diff.actual"
  local status="$TMP/large-diff.status"

  mkdir -p "$repo"
  git -C "$repo" init -q
  git -C "$repo" config user.email test@example.com
  git -C "$repo" config user.name test
  printf 'start\n' > "$repo/data.txt"
  git -C "$repo" add data.txt
  git -C "$repo" commit -qm base

  printf 'small evidence\n' >> "$repo/data.txt"
  git -C "$repo" diff HEAD -- "${MA_SAFE_PATHS[@]}" > "$expected"
  OMS_PROMPT_DIFF_BYTES=4096 ma_safe_diff "$repo" > "$actual"
  cmp -s "$expected" "$actual" || fail "small diff should pass through unchanged"

  awk 'BEGIN { for (i = 0; i < 600; i++) printf "diff-evidence-%04d-abcdefghijklmnopqrstuvwxyz0123456789\n", i }' >> "$repo/data.txt"
  OMS_PROMPT_DIFF_BYTES=512 ma_safe_diff "$repo" > "$capped"
  ma_safe_status "$repo" > "$status"
  assert_bounded "$capped" 512
  assert_contains "$capped" 'diff --git'
  assert_contains "$capped" 'small evidence'
  assert_contains "$capped" '[TRUNCATED: git diff omitted '
  assert_contains "$capped" 'OMS_PROMPT_DIFF_BYTES=512'
  assert_contains "$status" 'data.txt'
}

test_quoted_output_budget_and_small_passthrough() {
  local small="$TMP/small-quote"
  local small_out="$TMP/small-quote.out"
  local large="$TMP/large-quote"
  local capped="$TMP/large-quote.out"

  printf 'opening evidence\nsecond line\n' > "$small"
  OMS_PROMPT_QUOTE_BYTES=4096 ma_sanitize_quoted_output < "$small" > "$small_out"
  cmp -s "$small" "$small_out" || fail "small sanitized quote should pass through unchanged"

  {
    printf 'opening quote evidence\n'
    awk 'BEGIN { for (i = 0; i < 24; i++) printf "quote-evidence-%04d-abcdefghijklmnopqrstuvwxyz0123456789\n", i }'
  } > "$large"
  OMS_PROMPT_QUOTE_BYTES=384 ma_sanitize_quoted_output < "$large" > "$capped"
  assert_bounded "$capped" 384
  assert_contains "$capped" 'opening quote evidence'
  assert_contains "$capped" '[TRUNCATED: provider output omitted '
  assert_contains "$capped" 'OMS_PROMPT_QUOTE_BYTES=384'
}

test_debate_and_synthesis_use_quote_budget() {
  local self="$TMP/self.md"
  local other="$TMP/other.md"
  local debate="$TMP/debate.prompt"
  local synth="$TMP/synthesis.md"

  write_artifact "$self" 'SELF-BEGIN-EVIDENCE'
  write_artifact "$other" 'OTHER-BEGIN-EVIDENCE'

  PROMPT='Bound prior provider context.'
  MA_DEBATE_ROLE=advisors
  MA_DEBATE_TOPIC=question
  MA_DEBATE_SECTIONS='Answer:'
  OMS_PROMPT_QUOTE_BYTES=320
  write_debate_prompt "$debate" codex 2 "$self" "claude:$other"
  assert_contains "$debate" 'SELF-BEGIN-EVIDENCE'
  assert_contains "$debate" 'OTHER-BEGIN-EVIDENCE'
  [ "$(grep -Fc '[TRUNCATED: provider output omitted ' "$debate")" -eq 2 ] ||
    fail "debate should cap each quoted provider output"

  MA_KIND=ask
  MA_SHOW_REPO=0
  ok=2
  total=2
  DEBATE=0
  prompt_file="$small_prompt"
  artifacts=("$self" "$other")
  last_arts=("$self" "$other")
  provider_names=(codex claude)
  ma_write_synthesis "$synth"
  assert_contains "$synth" 'SELF-BEGIN-EVIDENCE'
  assert_contains "$synth" 'OTHER-BEGIN-EVIDENCE'
  [ "$(grep -Fc '[TRUNCATED: provider output omitted ' "$synth")" -eq 2 ] ||
    fail "synthesis should cap each quoted provider output"
}

small_prompt="$TMP/operator-prompt"
printf 'Operator question\n' > "$small_prompt"

test_budget_defaults_and_invalid_fallback
test_diff_budget_and_small_passthrough
test_quoted_output_budget_and_small_passthrough
test_debate_and_synthesis_use_quote_budget

echo 'prompt-budget-smoke: ok'
