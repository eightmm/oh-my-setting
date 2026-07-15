#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-context-core.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() {
  echo "context-core-smoke: $*" >&2
  exit 1
}

# shellcheck disable=SC1091
. "$ROOT/scripts/lib/oms-common.sh"
# shellcheck disable=SC1091
. "$ROOT/scripts/lib/agent-memory-common.sh"
# shellcheck disable=SC1091
. "$ROOT/scripts/lib/agent-task-common.sh"

test_context_body_obeys_hard_cap() {
  local repo="$TMP/context-repo"
  local task="$repo/task.md"
  local output="$repo/context.out"
  local body="$repo/body.out"
  local budget=520
  local bytes
  local i

  mkdir -p "$repo"
  {
    printf '## Goal\n\n'
    i=1
    while [ "$i" -le 80 ]; do
      printf 'Oversized goal detail %03d padding padding padding.\n' "$i"
      i=$((i + 1))
    done
    printf '\n## Verify\n\nrun focused verification\n'
    printf '\n## Current State\n\n'
    i=1
    while [ "$i" -le 80 ]; do
      printf -- '- state %03d padding padding padding padding\n' "$i"
      i=$((i + 1))
    done
    printf -- '- NEWEST-STATE-SENTINEL\n'
    printf '\n## Next Step\n\nNEXT-STEP-SENTINEL\n'
  } > "$task"

  OMS_AGENT_TASK_CONTEXT_CHARS="$budget" \
    agent_task_emit_context "$repo" "$task" > "$output"

  sed -n '/^## Goal$/,$p' "$output" > "$body"
  bytes="$(wc -c < "$body" | tr -d ' ')"
  [ "$bytes" -le "$budget" ] ||
    fail "task body exceeded hard cap: $bytes > $budget"
  for text in '## Goal' '## Verify' '## Next Step' 'NEWEST-STATE-SENTINEL' \
      'NEXT-STEP-SENTINEL' \
      '... task context truncated; older entries dropped; newest entries retained ...'; do
    grep -Fq "$text" "$body" || fail "capped task body omitted: $text"
  done
}

test_capped_context_keeps_valid_utf8() {
  local repo="$TMP/context-utf8"
  local task="$repo/task.md"
  local output="$repo/context.out"
  local i

  command -v iconv >/dev/null 2>&1 || return 0
  mkdir -p "$repo"
  {
    printf '## Goal\n\n'
    i=1
    while [ "$i" -le 100 ]; do
      printf '가'
      i=$((i + 1))
    done
    printf '\n'
    printf '## Verify\n\n검증 실행\n'
    printf '## Current State\n\n- 가장 최신 상태를 유지한다\n'
    printf '## Next Step\n\n다음 단계를 실행한다\n'
  } > "$task"

  OMS_AGENT_TASK_CONTEXT_CHARS=300 \
    agent_task_emit_context "$repo" "$task" > "$output"
  iconv -f UTF-8 -t UTF-8 "$output" >/dev/null 2>&1 ||
    fail "capped context contained invalid UTF-8"
}

test_shell_keyword_mentions_stay_general() {
  local repo="$TMP/general-shell"
  local style

  mkdir -p "$repo/tests"
  cat > "$repo/tests/harness-smoke.sh" <<'EOF'
#!/usr/bin/env bash
echo "torch DataLoader LightningModule"
printf 'import torch\n' > "$TMP/example.py"
test -f train.py
EOF
  style="$("$ROOT/scripts/detect-project-style.sh" "$repo")"
  [ "$style" = general ] || fail "shell keyword fixture was misclassified as $style"
}

test_real_ml_signals_are_ml() {
  local imports="$TMP/python-import"
  local entry="$TMP/ml-entry"
  local deps="$TMP/ml-dependency"

  mkdir -p "$imports" "$entry" "$deps"
  printf 'import torch\n' > "$imports/main.py"
  printf 'print("train")\n' > "$entry/train.py"
  printf 'torch>=2.0\n' > "$deps/requirements.txt"

  [ "$("$ROOT/scripts/detect-project-style.sh" "$imports")" = ml ] ||
    fail "Python torch import was not classified as ml"
  [ "$("$ROOT/scripts/detect-project-style.sh" "$entry")" = ml ] ||
    fail "train.py entry was not classified as ml"
  [ "$("$ROOT/scripts/detect-project-style.sh" "$deps")" = ml ] ||
    fail "declared torch dependency was not classified as ml"
}

test_context_body_obeys_hard_cap
test_capped_context_keeps_valid_utf8
test_shell_keyword_mentions_stay_general
test_real_ml_signals_are_ml
echo "context-core-smoke: ok"
